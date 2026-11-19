//! Recipe to IFC: generated parts leave as their semantic class (by role), groups as element
//! assemblies, the file validates clean and an import keeps every class.

use std::collections::{BTreeMap, HashMap};

use ddc_io::ifc::{export_ifc, import_ifc, validate_ifc};
use ddc_model::{Model, Recipe, RecipePart};
use ddc_ops::{Cat, GeoOp, Op, Ref};
use ddc_script::recipes::recipe;
use ddc_script::{GeoScript, RunOpts, Sandbox, ROLE_IFC};
use serde_json::json;

fn count(t: &str, e: &str) -> usize {
    t.matches(&format!("={e}(")).count()
}

fn classes_of(m: &Model) -> BTreeMap<String, usize> {
    let mut n = BTreeMap::new();
    for (_, s) in m.solids(None).unwrap() {
        let k = s.ifc.map_or("IfcBuildingElementProxy".to_string(), |c| c.class);
        *n.entry(k).or_default() += 1;
    }
    n
}

fn recipe_round_trip(name: &str) {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("Level 1", 0.0).unwrap();
    let o = RunOpts::new(Ref::name(Cat::Level, "Level 1"), json!({}));
    let run = Sandbox.run(recipe(name).unwrap(), &o).unwrap();

    // expected class per role: the role table where it names the role, else what the op says
    let mut by_role: BTreeMap<String, usize> = BTreeMap::new();
    let mut want: BTreeMap<String, usize> = BTreeMap::new();
    for op in &run.env.groups[0].ops {
        let Op::Geometry(GeoOp::Solid { name, ifc, .. }) = op else { continue };
        let role = name.as_deref().unwrap_or("").split(' ').next().unwrap_or("").to_string();
        let class = match ROLE_IFC.iter().find(|r| r.0 == role) {
            Some(&(_, c, p)) => {
                let got = ifc.as_ref().expect("a role in the table has a class");
                assert_eq!((got.class.as_str(), got.predefined.as_deref()), (c, Some(p)), "{role}");
                c.to_string()
            }
            None => ifc.as_ref().map_or("IfcBuildingElementProxy".into(), |c| c.class.clone()),
        };
        *by_role.entry(role).or_default() += 1;
        *want.entry(class).or_default() += 1;
    }
    assert!(by_role.len() >= 2, "{name}: {by_role:?}");
    assert!(
        want.keys().any(|k| k != "IfcBuildingElementProxy"),
        "{name}: no classed parts {want:?}"
    );

    let r = m.apply_envelope(&run.env).unwrap_or_else(|e| panic!("{name}: {e}"));
    let keys: HashMap<u32, &str> = run.parts.iter().map(|p| (p.slot, p.key.as_str())).collect();
    let parts: Vec<RecipePart> = r
        .created
        .iter()
        .filter_map(|(slot, rf)| match rf {
            Ref::Id { cat: Cat::Solid, id } => Some(RecipePart {
                key: keys.get(slot)?.to_string(),
                tbl: "solid".into(),
                el: *id,
            }),
            _ => None,
        })
        .collect();
    assert_eq!(parts.len(), run.parts.len(), "{name}: every part is a solid row");
    m.recipe_add(
        &Recipe {
            id: 0,
            name: name.into(),
            script: recipe(name).unwrap().into(),
            params_json: "{}".into(),
            hash: run.hash.clone(),
            version: ddc_script::STD_VERSION as i64,
        },
        &parts,
    )
    .unwrap();
    assert_eq!(classes_of(&m), want, "{name}: model rows");

    let t = export_ifc(&m).unwrap();
    // every group path and its ancestors is one assembly
    let mut groups = std::collections::BTreeSet::new();
    for p in &parts {
        let mut path = p.key.as_str();
        while let Some((up, _)) = path.rsplit_once('/') {
            groups.insert(up.to_string());
            path = up;
        }
    }
    for (class, n) in &want {
        let e = class.to_ascii_uppercase();
        let asm = if e == "IFCELEMENTASSEMBLY" { groups.len() } else { 0 };
        assert_eq!(count(&t, &e), n + asm, "{name}: {class}");
    }
    if !want.contains_key("IfcElementAssembly") {
        assert_eq!(count(&t, "IFCELEMENTASSEMBLY"), groups.len(), "{name}: {groups:?}");
    }
    let aggs = t.lines().filter(|x| x.contains("=IFCRELAGGREGATES(")).count();
    assert!(aggs >= 3 + groups.len(), "{name}: {aggs} aggregations");
    let rep = validate_ifc(t.as_bytes());
    assert!(rep.is_clean(), "{name}: {}", rep.brief(10));

    let mut back = Model::open(":memory:").unwrap();
    let ir = import_ifc(&mut back, t.as_bytes()).unwrap();
    assert_eq!(ir.meshes, 0, "{name}: {ir:?}");
    assert_eq!(classes_of(&back), want, "{name}: classes after import");
}

#[test]
fn every_role_class_is_on_the_allow_list() {
    for (role, class, pre) in ROLE_IFC {
        let c = ddc_model::SolidIfc::new(class, Some(*pre)).unwrap_or_else(|e| panic!("{role}: {e}"));
        assert_eq!((c.class.as_str(), c.predefined.as_str()), (*class, *pre), "{role}");
    }
}

#[test]
fn diagrid_tower_parts_keep_their_classes_through_ifc() {
    recipe_round_trip("twisted_diagrid_tower");
}

#[test]
fn louvres_leave_as_shading_devices() {
    recipe_round_trip("sun_louvres");
}

#[test]
fn footbridge_parts_keep_their_classes_through_ifc() {
    recipe_round_trip("cable_stayed_footbridge");
}
