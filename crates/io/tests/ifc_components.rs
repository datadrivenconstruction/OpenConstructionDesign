// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Components through IFC: class by category, a shared representation map, exact round-trip.

use ddc_io::ifc::{export_ifc, import_ifc};
use ddc_model::{CompHost, CompSpec, LayerSpec, Model, ParamValue, SlabSpec, WallSpec};

type Snap = Vec<(String, String, String, [i64; 5], bool, String, i64)>;

/// Family, type, values, world placement (in nm/µrad), host, volume (in mm³).
fn snapshot(m: &Model) -> Snap {
    let mut v: Snap = m
        .component_schedule()
        .unwrap()
        .iter()
        .map(|r| {
            let q = m.component_quantities(r.id).unwrap();
            let p = q.placement;
            let k = |x: f64| (x * 1e9).round() as i64;
            let host = match r.host {
                CompHost::None => "-",
                CompHost::Wall(_) => "wall",
                CompHost::Floor(_) => "floor",
                CompHost::Ceiling(_) => "ceiling",
            };
            let vals = format!("{:?}", m.component(r.id).unwrap().values);
            (
                r.family.clone(),
                r.type_name.clone(),
                vals,
                [
                    k(p.origin.0),
                    k(p.origin.1),
                    k(p.origin.2),
                    k(p.rotation),
                    r.level,
                ],
                p.mirror,
                host.to_string(),
                (q.volume * 1e9).round() as i64,
            )
        })
        .collect();
    v.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
    v
}

fn count(t: &str, ent: &str) -> usize {
    t.matches(&format!("={ent}(")).count()
}

#[test]
fn components_round_trip_exactly() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 3.0).unwrap();
    let wt = m
        .add_wall_type(
            "W200",
            &[LayerSpec {
                material: "Brick".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let wall = m
        .add_wall(&WallSpec {
            a: (0.0, 0.0),
            b: (4.0, 0.0),
            level: lv,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .unwrap();
    let st = m
        .add_slab_type(
            "S200",
            &[LayerSpec {
                material: "Concrete".into(),
                thickness: 0.2,
            }],
        )
        .unwrap();
    let slab = m
        .add_slab(&SlabSpec {
            level: lv,
            slab_type: st,
            offset: 0.05,
            outer: vec![(0.0, 0.5), (6.0, 0.5), (6.0, 6.0), (0.0, 6.0)],
            holes: vec![],
        })
        .unwrap();
    let ty = |m: &mut Model, f: &str, t: &str| {
        let id = m.load_library_family(f).unwrap();
        m.comp_type_id(id, t).unwrap().unwrap()
    };
    let table = ty(&mut m, "Table", "1600x800");
    let basin = ty(&mut m, "Washbasin", "500");
    let radiator = ty(&mut m, "Radiator", "1000x500");
    let bed = ty(&mut m, "Bed", "Double 1600x2000");
    let mut specs = vec![];
    for k in 0..3 {
        let mut s = CompSpec::at(lv, table, 1.0 + 2.0 * k as f64, 3.0);
        s.rotation = 0.3 * k as f64;
        s.mirror = k == 1;
        specs.push(s);
    }
    let mut s = CompSpec::at(lv, basin, 1.0, 0.1);
    s.host = CompHost::Wall(wall);
    s.values = vec![("Rim".into(), ParamValue::Num(0.8))];
    specs.push(s);
    let mut s = CompSpec::at(lv, radiator, 2.5, 0.1);
    s.host = CompHost::Wall(wall);
    specs.push(s);
    let mut s = CompSpec::at(lv, bed, 4.0, 4.0);
    s.host = CompHost::Floor(slab);
    s.rotation = 1.0;
    specs.push(s);
    m.add_components(&specs).unwrap();

    let text = export_ifc(&m).unwrap();
    assert_eq!(count(&text, "IFCFURNITURE"), 4);
    assert_eq!(count(&text, "IFCSANITARYTERMINAL"), 1);
    assert_eq!(count(&text, "IFCSPACEHEATER"), 1);
    // three tables of one type — one map; total maps equal the number of distinct (type, values)
    assert_eq!(count(&text, "IFCREPRESENTATIONMAP"), 4);
    assert_eq!(count(&text, "IFCMAPPEDITEM"), 6);
    assert_eq!(
        count(&text, "IFCCARTESIANTRANSFORMATIONOPERATOR3DNONUNIFORM"),
        1
    );
    assert!(text.contains(".RADIATOR."));

    let mut dst = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
    assert_eq!(rep.components, 6);
    assert!(rep.skipped.is_empty(), "{:?}", rep.skipped);
    assert_eq!(snapshot(&dst), snapshot(&m));
    assert_eq!(dst.families().unwrap().len(), 4);
    assert!(dst.check_integrity().unwrap().is_empty());
    // hosts are preserved: deleting a wall removes the wall-hosted ones
    let w: i64 = dst
        .sql()
        .query_row("SELECT id FROM wall", [], |r| r.get(0))
        .unwrap();
    dst.delete_wall(w).unwrap();
    assert_eq!(dst.components(None).unwrap().len(), 4);
    dst.undo().unwrap();
    // a repeated import reuses families and types
    import_ifc(&mut dst, text.as_bytes()).unwrap();
    assert_eq!(dst.families().unwrap().len(), 4);
    // same GlobalIds: re-import updates the rows in place instead of duplicating them
    assert_eq!(dst.components(None).unwrap().len(), 6);
}

fn line_of<'a>(t: &'a str, ent: &str) -> Vec<&'a str> {
    t.lines().filter(|l| l.contains(&format!("={ent}("))).collect()
}

/// Every component type goes as its own type object of the subtype of its class, with its
/// representation map, a property set of resolved type values, IfcRelDefinesByType to its
/// instances and the classification reference of a `Classification` value. Export → import keeps
/// family, type and values; the re-export writes the same type objects.
#[test]
fn component_types_with_property_sets_and_classification_round_trip() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let shelf = "family Shelf Unit\ncategory casework\nhost floor\nifc IfcFurniture SHELF\n\
        param Width length type 900mm\nparam Depth length type 400mm\nparam Height length type 1800mm\n\
        param Shelves int type 5\nparam Classification text type \"Uniclass 2015: Pr_40_30_25\"\n\
        type \"900\" Width=900mm\ntype \"1200\" Width=1200mm\n\
        extrude Body rect(-Width / 2, -Depth / 2, Width / 2, Depth / 2) z(0, Height)\n\
        symbol rect(-Width / 2, -Depth / 2, Width / 2, Depth / 2)\n";
    let sf = m.load_family(shelf).unwrap();
    let s900 = m.comp_type_id(sf, "900").unwrap().unwrap();
    let s1200 = m.comp_type_id(sf, "1200").unwrap().unwrap();
    let tf = m.load_library_family("Table").unwrap();
    let table = m.comp_type_id(tf, "1600x800").unwrap().unwrap();
    let lf = m.load_library_family("Ceiling Light").unwrap();
    let light = m.comp_types(lf).unwrap()[0].0;
    let mut specs = vec![];
    for (k, t) in [s900, s900, s1200, table, light].into_iter().enumerate() {
        specs.push(CompSpec::at(lv, t, 1.5 * k as f64, 2.0));
    }
    m.add_components(&specs).unwrap();

    let text = export_ifc(&m).unwrap();
    let rep = ddc_io::ifc::validate_ifc(text.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    // one type object per type: two shelves and the table are furniture, the light its own class
    let fts = line_of(&text, "IFCFURNITURETYPE");
    assert_eq!(fts.len(), 3, "{fts:?}");
    assert_eq!(line_of(&text, "IFCLIGHTFIXTURETYPE").len(), 1);
    assert_eq!(count(&text, "IFCRELDEFINESBYTYPE"), 4);
    let t900 = fts.iter().find(|l| l.contains(",'900',")).expect("type 900");
    assert!(t900.ends_with(",'Shelf Unit',.NOTDEFINED.,.SHELF.);"), "{t900}");
    let tt = fts.iter().find(|l| l.contains(",'1600x800',")).expect("table type");
    assert!(tt.ends_with(",'Table',.NOTDEFINED.,.TABLE.);"), "{tt}");
    // the shelf type relates both of its instances, and carries its map and value set
    let id_of = |l: &str| l[1..l.find('=').unwrap()].to_string();
    let rel = line_of(&text, "IFCRELDEFINESBYTYPE")
        .into_iter()
        .find(|l| l.ends_with(&format!(",#{});", id_of(t900))))
        .expect("rel of type 900");
    let refs_of = |l: &str| l[l.find('=').unwrap()..].matches('#').count();
    assert_eq!(refs_of(rel), 2 + 1, "two instances and the type: {rel}");
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(0.9),$)"));
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(1.2),$)"));
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Shelves',$,IFCINTEGER(5),$)"));
    assert!(text.contains("IFCPROPERTYSINGLEVALUE('Width',$,IFCLENGTHMEASURE(1.6),$)"));
    assert_eq!(text.matches("'DDC_ComponentType'").count(), 4);
    // classification: one system, one reference per shelf type, tied to the type objects
    let cls = line_of(&text, "IFCCLASSIFICATION");
    assert_eq!(cls.len(), 1, "{cls:?}");
    assert!(cls[0].contains("'Uniclass 2015'"));
    let refs = line_of(&text, "IFCCLASSIFICATIONREFERENCE");
    assert_eq!(refs.len(), 2);
    assert!(refs.iter().all(|l| l.contains("($,'Pr_40_30_25',$,#")), "{refs:?}");
    assert_eq!(count(&text, "IFCRELASSOCIATESCLASSIFICATION"), 2);

    // round trip: family, type and values come back; a re-export writes the same types
    let mut dst = Model::open(":memory:").unwrap();
    let rep = import_ifc(&mut dst, text.as_bytes()).unwrap();
    assert_eq!(rep.components, 5);
    assert_eq!(snapshot(&dst), snapshot(&m));
    let df = dst.family_id("Shelf Unit").unwrap().unwrap();
    let names: Vec<String> = dst.comp_types(df).unwrap().into_iter().map(|t| t.1).collect();
    assert_eq!(names, ["900", "1200"]);
    for (a, b) in [(s1200, "1200"), (s900, "900")] {
        let d = dst.comp_type_id(df, b).unwrap().unwrap();
        assert_eq!(dst.comp_type_values(d).unwrap(), m.comp_type_values(a).unwrap(), "{b}");
    }
    let again = export_ifc(&dst).unwrap();
    for ent in ["IFCFURNITURETYPE", "IFCLIGHTFIXTURETYPE", "IFCCLASSIFICATIONREFERENCE"] {
        let strip = |t: &str| -> Vec<String> {
            let mut v: Vec<String> = line_of(t, ent)
                .into_iter()
                .map(|l| {
                    // entity numbers and generated GlobalIds differ between files
                    let body = &l[l.find(',').unwrap_or(0)..];
                    body.split('#').map(|p| p.trim_start_matches(|c: char| c.is_ascii_digit())).collect()
                })
                .collect();
            v.sort();
            v
        };
        assert_eq!(strip(&again), strip(&text), "{ent}");
    }

    // IFC2X3: the furniture type keeps AssemblyPlace and drops PredefinedType
    let (old, _) = ddc_io::ifc::export_ifc_with(
        &m,
        &ddc_io::ifc::IfcOptions {
            schema: ddc_io::ifc::IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let f2 = line_of(&old, "IFCFURNITURETYPE");
    assert_eq!(f2.len(), 3);
    assert!(f2.iter().all(|l| l.ends_with(",.NOTDEFINED.);")), "{f2:?}");
    assert!(line_of(&old, "IFCLIGHTFIXTURETYPE")[0].ends_with(",.NOTDEFINED.);"));
    assert!(line_of(&old, "IFCCLASSIFICATIONREFERENCE").iter().all(|l| l.ends_with(");") && l.matches(',').count() == 3));
}

/// Library families of lift, pump, valve, tank, heat exchanger, column and beam classes go with
/// type objects of their own class and valid predefined types. The lift is valid in IFC2X3 too,
/// which has no LIFTINGGEAR: its type there is NOTDEFINED.
#[test]
fn equipment_and_lift_families_export_their_own_types() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let cases = [
        ("Fire Pump", "IFCPUMPTYPE"),
        ("Centrifugal Pump", "IFCPUMPTYPE"),
        ("Gate Valve", "IFCVALVETYPE"),
        ("Pressure Vessel", "IFCTANKTYPE"),
        ("Heat Exchanger", "IFCHEATEXCHANGERTYPE"),
        ("Glulam Column", "IFCCOLUMNTYPE"),
        ("Glulam Beam", "IFCBEAMTYPE"),
    ];
    let mut specs = vec![];
    for (k, (fam, _)) in cases.iter().enumerate() {
        let f = m.load_library_family(fam).unwrap();
        specs.push(CompSpec::at(lv, m.comp_types(f).unwrap()[0].0, 3.0 * k as f64, 5.0));
    }
    m.add_components(&specs).unwrap();
    let text = export_ifc(&m).unwrap();
    let rep = ddc_io::ifc::validate_ifc(text.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    for ty in ["IFCVALVETYPE", "IFCTANKTYPE", "IFCHEATEXCHANGERTYPE", "IFCCOLUMNTYPE", "IFCBEAMTYPE"] {
        assert_eq!(line_of(&text, ty).len(), 1, "{ty}");
    }
    assert_eq!(line_of(&text, "IFCPUMPTYPE").len(), 2);
    assert_eq!(count(&text, "IFCBUILDINGELEMENTPROXYTYPE"), 0);
    assert_eq!(count(&text, "IFCRELDEFINESBYTYPE"), cases.len());

    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let f = m.load_library_family("Platform Lift").unwrap();
    let t = m.comp_types(f).unwrap()[0].0;
    m.add_components(&[CompSpec::at(lv, t, 1.0, 1.0)]).unwrap();
    let text = export_ifc(&m).unwrap();
    let rep = ddc_io::ifc::validate_ifc(text.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    let ty = line_of(&text, "IFCTRANSPORTELEMENTTYPE");
    assert_eq!(ty.len(), 1, "{ty:?}");
    assert!(ty[0].ends_with(",.LIFTINGGEAR.);"), "{ty:?}");
    assert_eq!(count(&text, "IFCBUILDINGELEMENTPROXYTYPE"), 0);
    assert_eq!(count(&text, "IFCRELDEFINESBYTYPE"), 1);
    let (old, _) = ddc_io::ifc::export_ifc_with(
        &m,
        &ddc_io::ifc::IfcOptions {
            schema: ddc_io::ifc::IfcSchema::Ifc2x3,
            timestamp: Some("2026-01-01T00:00:00".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let rep = ddc_io::ifc::validate_ifc(old.as_bytes());
    assert!(rep.is_clean(), "{:?}", rep.errors);
    let ty = line_of(&old, "IFCTRANSPORTELEMENTTYPE");
    assert_eq!(ty.len(), 1, "{ty:?}");
    assert!(ty[0].ends_with(",.NOTDEFINED.);"), "{ty:?}");
    let occ = line_of(&old, "IFCTRANSPORTELEMENT");
    assert_eq!(occ.len(), 1, "{occ:?}");
    assert!(occ[0].ends_with(",.NOTDEFINED.,$,$);"), "{occ:?}");
}

/// The planting set is read from the family's own `plant` solid, whatever its parameters are called.
#[test]
fn a_plant_family_exports_its_planting_values_by_any_parameter_names() {
    let mut m = Model::open(":memory:").unwrap();
    let lv = m.add_level("L1", 0.0).unwrap();
    let src = "family Hedge Tree\ncategory planting\nhost floor\nifc IfcGeographicElement NOTDEFINED\n\
        param Shape int type 3\nparam H length type 9000mm\nparam W length type 3000mm\n\
        param Bole length type 250mm\nparam Bare length type 1500mm\nparam Fullness number type 0.6\n\
        param Id int type 42\nparam Wood material type \"Bark Brown\"\nparam Cover material type \"Foliage Conifer\"\n\
        type \"Tall\" H=9000mm\n\
        plant Stem part(wood) form(Shape) height(H) crown(W) trunk(Bole) clear(Bare) leaf(Fullness) seed(Id) mat(Wood) show(plan, 3d, elev)\n\
        plant Crown part(foliage) form(Shape) height(H) crown(W) trunk(Bole) clear(Bare) leaf(Fullness) seed(Id) mat(Cover) show(plan, 3d, elev)\n";
    let f = m.load_family(src).unwrap();
    let t = m.comp_type_id(f, "Tall").unwrap().unwrap();
    m.add_components(&[CompSpec::at(lv, t, 1.0, 1.0)]).unwrap();
    let text = export_ifc(&m).unwrap();
    assert!(ddc_io::ifc::validate_ifc(text.as_bytes()).is_clean());
    assert_eq!(text.matches("'DDC_Planting'").count(), 1);
    for p in [
        "IFCPROPERTYSINGLEVALUE('Form',$,IFCLABEL('columnar'),$)",
        "IFCPROPERTYSINGLEVALUE('Height',$,IFCLENGTHMEASURE(9.),$)",
        "IFCPROPERTYSINGLEVALUE('CrownWidth',$,IFCLENGTHMEASURE(3.),$)",
        "IFCPROPERTYSINGLEVALUE('TrunkDiameter',$,IFCLENGTHMEASURE(0.25),$)",
        "IFCPROPERTYSINGLEVALUE('ClearTrunkHeight',$,IFCLENGTHMEASURE(1.5),$)",
        "IFCPROPERTYSINGLEVALUE('FoliageFullness',$,IFCREAL(0.6),$)",
        "IFCPROPERTYSINGLEVALUE('GrowthSeed',$,IFCINTEGER(42),$)",
        "IFCPROPERTYSINGLEVALUE('BarkMaterial',$,IFCLABEL('Bark Brown'),$)",
        "IFCPROPERTYSINGLEVALUE('FoliageMaterial',$,IFCLABEL('Foliage Conifer'),$)",
    ] {
        assert!(text.contains(p), "missing {p}");
    }
}
