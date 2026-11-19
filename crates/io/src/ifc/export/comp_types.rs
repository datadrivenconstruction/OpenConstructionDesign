// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Component types: one type object per component type, of the subtype that matches the
//! occurrence class (IfcFurnitureType for IfcFurniture, ...), with its representation map, the
//! `DDC_ComponentType` property set of resolved type values and IfcRelDefinesByType to every
//! placed instance. A text type parameter `Classification` («System: Code» or a bare code) gives an
//! IfcClassificationReference on the type; one IfcClassification per system.

use std::collections::BTreeMap;

use ddc_model::{Model, ParamKind, ParamValue};

use super::super::step::{list, r, real, text};
use super::{s, Ctx, NONE};

/// Instances and the representation map of one component type, gathered over all storeys.
#[derive(Debug, Default)]
pub(super) struct Members {
    pub members: Vec<u32>,
    pub map: Option<u32>,
}

/// Name of the type parameter that carries a classification code.
pub const CLASSIFICATION_PARAM: &str = "Classification";

/// IFC4 type class of an occurrence class (as the family names it) and whether it has the
/// AssemblyPlace attribute before PredefinedType. `None`: the class has no type object of its own
/// and the type goes as IfcBuildingElementProxyType.
pub fn type_class(occurrence: &str) -> Option<(&'static str, bool)> {
    Some(match occurrence.to_ascii_uppercase().as_str() {
        "IFCFURNITURE" => ("IFCFURNITURETYPE", true),
        "IFCSYSTEMFURNITUREELEMENT" => ("IFCSYSTEMFURNITUREELEMENTTYPE", false),
        "IFCBUILDINGELEMENTPROXY" => ("IFCBUILDINGELEMENTPROXYTYPE", false),
        "IFCSANITARYTERMINAL" => ("IFCSANITARYTERMINALTYPE", false),
        "IFCLIGHTFIXTURE" => ("IFCLIGHTFIXTURETYPE", false),
        "IFCAIRTERMINAL" => ("IFCAIRTERMINALTYPE", false),
        "IFCAIRTERMINALBOX" => ("IFCAIRTERMINALBOXTYPE", false),
        "IFCCOVERING" => ("IFCCOVERINGTYPE", false),
        "IFCMEMBER" => ("IFCMEMBERTYPE", false),
        "IFCRAILING" => ("IFCRAILINGTYPE", false),
        "IFCSHADINGDEVICE" => ("IFCSHADINGDEVICETYPE", false),
        "IFCSLAB" => ("IFCSLABTYPE", false),
        "IFCGEOGRAPHICELEMENT" => ("IFCGEOGRAPHICELEMENTTYPE", false),
        "IFCOUTLET" => ("IFCOUTLETTYPE", false),
        "IFCELECTRICDISTRIBUTIONBOARD" => ("IFCELECTRICDISTRIBUTIONBOARDTYPE", false),
        "IFCSPACEHEATER" => ("IFCSPACEHEATERTYPE", false),
        "IFCSWITCHINGDEVICE" => ("IFCSWITCHINGDEVICETYPE", false),
        "IFCUNITARYEQUIPMENT" => ("IFCUNITARYEQUIPMENTTYPE", false),
        "IFCFIRESUPPRESSIONTERMINAL" => ("IFCFIRESUPPRESSIONTERMINALTYPE", false),
        "IFCWASTETERMINAL" => ("IFCWASTETERMINALTYPE", false),
        "IFCLAMP" => ("IFCLAMPTYPE", false),
        "IFCELECTRICAPPLIANCE" => ("IFCELECTRICAPPLIANCETYPE", false),
        "IFCMEDICALDEVICE" => ("IFCMEDICALDEVICETYPE", false),
        "IFCSTAIR" => ("IFCSTAIRTYPE", false),
        "IFCPIPEFITTING" => ("IFCPIPEFITTINGTYPE", false),
        "IFCDUCTFITTING" => ("IFCDUCTFITTINGTYPE", false),
        "IFCDOOR" => ("IFCDOORTYPE", false),
        "IFCWINDOW" => ("IFCWINDOWTYPE", false),
        _ => return None,
    })
}

/// PredefinedType and the attributes after it. A door family may name its operation
/// (SLIDING_TO_LEFT, ...) in place of the type: it goes to OperationType.
fn predefined_and_tail(type_class: &str, pre: &str) -> (String, Vec<String>) {
    let e = |v: &str| format!(".{v}.");
    match type_class {
        "IFCDOORTYPE" => {
            let own = matches!(pre, "DOOR" | "GATE" | "TRAPDOOR" | "USERDEFINED" | "NOTDEFINED");
            let (ty, op) = if own { (pre, "NOTDEFINED") } else { ("DOOR", pre) };
            (e(ty), vec![e(op), s(NONE), s(NONE)])
        }
        // PartitioningType, ParameterTakesPrecedence, UserDefinedPartitioningType
        "IFCWINDOWTYPE" => (e(pre), vec![e("NOTDEFINED"), s(NONE), s(NONE)]),
        _ => (e(pre), vec![]),
    }
}

/// (system, code) of a classification value: «System: Code» or a bare code.
pub fn classification(v: &str) -> Option<(String, String)> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    Some(match v.split_once(':') {
        Some((sys, code)) if !sys.trim().is_empty() && !code.trim().is_empty() => {
            (sys.trim().to_string(), code.trim().to_string())
        }
        _ => ("Classification".to_string(), v.to_string()),
    })
}

/// Typed IFC value of a resolved parameter.
fn measure(kind: ParamKind, num: Option<f64>, txt: Option<&str>) -> Option<String> {
    Some(match (kind, num, txt) {
        (ParamKind::Length, Some(v), _) => format!("IFCLENGTHMEASURE({})", real(v)),
        (ParamKind::Area, Some(v), _) => format!("IFCAREAMEASURE({})", real(v)),
        (ParamKind::Volume, Some(v), _) => format!("IFCVOLUMEMEASURE({})", real(v)),
        (ParamKind::Angle, Some(v), _) => format!("IFCPLANEANGLEMEASURE({})", real(v)),
        (ParamKind::Int, Some(v), _) => format!("IFCINTEGER({})", v.round() as i64),
        (ParamKind::Bool, Some(v), _) => {
            format!("IFCBOOLEAN({})", if v != 0.0 { ".T." } else { ".F." })
        }
        (_, Some(v), _) if v.is_finite() => format!("IFCREAL({})", real(v)),
        (_, _, Some(t)) => format!("IFCLABEL({})", text(t)),
        _ => return None,
    })
}

/// The type objects of every component type that has a placed instance.
pub(super) fn write(
    c: &mut Ctx,
    model: &Model,
    types: &BTreeMap<i64, Members>,
) -> Result<(), String> {
    let mut systems: BTreeMap<String, u32> = BTreeMap::new();
    for (&ctype, t) in types {
        if t.members.is_empty() {
            continue;
        }
        let (fid, tname): (i64, String) = model
            .sql()
            .query_row(
                "SELECT family, name FROM comp_type WHERE id=?1",
                [ctype],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        let fam = model.family(fid).map_err(|e| e.to_string())?;
        let tv: Vec<(String, ParamValue)> =
            model.comp_type_values(ctype).map_err(|e| e.to_string())?;
        let res = fam.resolve(&tv, &[]).map_err(|e| e.to_string())?;
        let mut props = vec![];
        let mut code = None;
        for p in fam.params.iter().filter(|p| !p.instance) {
            let num = res.nums.get(&p.name).map(|q| q.v).filter(|v| v.is_finite());
            let txt = res.texts.get(&p.name).map(String::as_str);
            if p.name.eq_ignore_ascii_case(CLASSIFICATION_PARAM) {
                code = txt.and_then(classification);
            }
            if let Some(v) = measure(p.kind, num, txt) {
                props.push(c.s.add(
                    "IFCPROPERTYSINGLEVALUE",
                    &[text(&p.name), s(NONE), v, s(NONE)],
                ));
            }
        }
        let pset = (!props.is_empty()).then(|| {
            c.rooted(
                "IFCPROPERTYSET",
                "comp-type-pset",
                ctype,
                "DDC_ComponentType",
                &[list(&props)],
            )
        });
        let (class, pre) = &fam.ifc;
        let (type_class, assembly, pre) = match type_class(class) {
            Some((tc, a)) => (tc, a, pre.as_str()),
            None => ("IFCBUILDINGELEMENTPROXYTYPE", false, "NOTDEFINED"),
        };
        let pre = if pre.is_empty() { "NOTDEFINED" } else { pre };
        let mut rest = vec![
            s(NONE),
            pset.map_or_else(|| s(NONE), |p| list(&[p])),
            t.map.map_or_else(|| s(NONE), |m| list(&[m])),
            s(NONE),
            text(&fam.name),
        ];
        if assembly {
            // AssemblyPlace is required (FACTORY, SITE or NOTDEFINED)
            rest.push(".NOTDEFINED.".into());
        }
        let (pre, tail) = predefined_and_tail(type_class, pre);
        rest.push(pre);
        rest.extend(tail);
        let ty = c.rooted(type_class, "comp-type", ctype, &tname, &rest);
        c.rel(
            "IFCRELDEFINESBYTYPE",
            "comp-type-rel",
            ctype,
            &[list(&t.members), r(ty)],
        );
        if let Some((sys, code)) = code {
            let cls = match systems.get(&sys) {
                Some(&x) => x,
                None => {
                    let x = c.s.add(
                        "IFCCLASSIFICATION",
                        &[s(NONE), text(""), s(NONE), text(&sys), s(NONE), s(NONE), s(NONE)],
                    );
                    systems.insert(sys.clone(), x);
                    x
                }
            };
            let refe = c.s.add(
                "IFCCLASSIFICATIONREFERENCE",
                &[s(NONE), text(&code), s(NONE), r(cls), s(NONE), s(NONE)],
            );
            c.rel(
                "IFCRELASSOCIATESCLASSIFICATION",
                "comp-type-class",
                ctype,
                &[list(&[ty]), r(refe)],
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_default_class_has_a_type_class() {
        for c in ddc_model::CompCategory::ALL {
            let (class, _) = c.ifc();
            assert!(type_class(class).is_some(), "{class}");
        }
        for f in ddc_model::library().unwrap() {
            assert!(type_class(&f.ifc.0).is_some(), "{}: {}", f.name, f.ifc.0);
        }
        assert_eq!(type_class("IfcFurniture"), Some(("IFCFURNITURETYPE", true)));
        assert_eq!(type_class("IfcUnknownThing"), None);
    }

    #[test]
    fn door_operation_goes_to_operation_type() {
        let t = |c, p| predefined_and_tail(c, p);
        assert_eq!(t("IFCDOORTYPE", "SLIDING_TO_LEFT").0, ".DOOR.");
        assert_eq!(t("IFCDOORTYPE", "SLIDING_TO_LEFT").1[0], ".SLIDING_TO_LEFT.");
        assert_eq!(t("IFCDOORTYPE", "GATE"), (".GATE.".into(), vec![".NOTDEFINED.".into(), "$".into(), "$".into()]));
        assert_eq!(t("IFCWINDOWTYPE", "WINDOW").1.len(), 3);
        assert_eq!(t("IFCMEMBERTYPE", "MULLION"), (".MULLION.".into(), vec![]));
    }

    #[test]
    fn classification_values() {
        assert_eq!(
            classification("Uniclass 2015: Pr_40_50_12"),
            Some(("Uniclass 2015".into(), "Pr_40_50_12".into()))
        );
        assert_eq!(
            classification("23-21 11 13"),
            Some(("Classification".into(), "23-21 11 13".into()))
        );
        assert_eq!(classification("  "), None);
        assert_eq!(
            classification(":x"),
            Some(("Classification".into(), ":x".into()))
        );
    }
}
