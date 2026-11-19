// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Components in DXF: one block per type (plus one per plan-changing variant) with hidden
//! attribute definitions, one INSERT per instance on its category layer with filled attributes.
//! Written, read back and expanded, the inserts land on the model's own plan symbols.

use ddc_io::dxf::{
    append_components, attr_tag, category_layer, expand_inserts, read_str, write_string,
    DxfDocument, Entity, ExpandOptions, HeaderVar, Pair,
};
use ddc_model::{
    CompSpec, Model, PVal, ParamSet, ParamSpec, ParamType, ParamValue, CLASSIFICATION_PREFIX,
};

struct Fx {
    m: Model,
    level: i64,
    t1: i64,
    t2: i64,
    wc: i64,
}

fn fx() -> Fx {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let tf = m.load_library_family("Table").unwrap();
    let tt = m.comp_types(tf).unwrap()[0].0;
    let t1 = m.add_component(&CompSpec::at(level, tt, 1.0, 1.0)).unwrap();
    let mut s = CompSpec::at(level, tt, 5.0, 2.0);
    s.rotation = std::f64::consts::FRAC_PI_2;
    s.mirror = true;
    let t2 = m.add_component(&s).unwrap();
    let wf = m.load_library_family("Wardrobe").unwrap();
    let wt = m.comp_types(wf).unwrap()[0].0;
    let wc = m.add_component(&CompSpec::at(level, wt, 8.0, 0.0)).unwrap();
    let pid = m
        .add_param(&ParamSpec {
            name: format!("{CLASSIFICATION_PREFIX}Uniclass"),
            ptype: ParamType::Text,
            per_type: false,
            unit: String::new(),
            group: String::new(),
            guid: None,
            categories: vec!["comp".into()],
        })
        .unwrap();
    m.set_param_values(&[ParamSet {
        param: pid,
        kind: "comp".into(),
        id: t1,
        value: Some(PVal::Text("Pr_40_50_12".into())),
    }])
    .unwrap();
    Fx { m, level, t1, t2, wc }
}

fn inserts(doc: &DxfDocument) -> Vec<&ddc_io::dxf::Insert> {
    doc.entities
        .iter()
        .filter_map(|e| match e {
            Entity::Insert(i) => Some(i),
            _ => None,
        })
        .collect()
}

fn attr<'a>(i: &'a ddc_io::dxf::Insert, tag: &str) -> &'a str {
    &i.attribs.iter().find(|a| a.tag == tag).unwrap_or_else(|| panic!("no {tag}")).value
}

#[test]
fn blocks_per_type_inserts_per_instance_round_trip() {
    let f = fx();
    let mut doc = DxfDocument::default();
    let rep = append_components(&mut doc, &f.m).unwrap();
    assert_eq!((rep.blocks, rep.inserts), (2, 3));
    let back = read_str(&write_string(&doc)).unwrap();
    assert_eq!(back.blocks.iter().filter(|b| b.name.contains(" - ")).count(), 2);
    let table = back.blocks.iter().find(|b| b.name == "Table - 1600x800").unwrap();
    assert!(!table.entities.is_empty());
    let tags: Vec<&str> = table.attdefs.iter().map(|a| a.tag.as_str()).collect();
    for t in ["ID", "FAMILY", "TYPE", "CATEGORY", "IFC_CLASS", "IFC_TYPE", "WIDTH", "SEATS", "CLASS_UNICLASS"] {
        assert!(tags.contains(&t), "{t} in {tags:?}");
    }
    assert!(table.attdefs.iter().all(|a| a.flags & 1 == 1), "attributes are hidden");
    let ins = inserts(&back);
    assert_eq!(ins.len(), 3);
    let by = |id: i64| *ins.iter().find(|i| attr(i, "ID") == id.to_string()).unwrap();
    let a = by(f.t1);
    assert_eq!(a.block, "Table - 1600x800");
    assert_eq!(a.common.layer, category_layer("furniture"));
    assert_eq!((attr(a, "FAMILY"), attr(a, "TYPE")), ("Table", "1600x800"));
    assert_eq!((attr(a, "IFC_CLASS"), attr(a, "IFC_TYPE")), ("IfcFurniture", "TABLE"));
    assert_eq!((attr(a, "WIDTH"), attr(a, "SEATS")), ("1.6", "4"));
    assert_eq!(attr(a, "WOOD"), "Oak");
    assert_eq!(attr(a, "CLASS_UNICLASS"), "Pr_40_50_12");
    let b = by(f.t2);
    assert_eq!(b.block, a.block, "same type, same block");
    assert_eq!(attr(b, "CLASS_UNICLASS"), "");
    assert!((b.rotation - 90.0).abs() < 1e-9 && b.scale[0] == -1.0);
    let w = by(f.wc);
    assert_eq!(w.common.layer, category_layer("casework"));
    assert!(back.layers.iter().any(|l| l.name == "COMP-CASEWORK"));
}

#[test]
fn expanded_inserts_match_the_model_plan_symbols() {
    let f = fx();
    let mut doc = DxfDocument::default();
    append_components(&mut doc, &f.m).unwrap();
    let back = read_str(&write_string(&doc)).unwrap();
    let flat = expand_inserts(&back, &ExpandOptions::default());
    let mut got: Vec<[i64; 4]> = flat
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::Line(l) => Some(key(l.start, l.end)),
            _ => None,
        })
        .collect();
    let level = f.level;
    let mut want: Vec<[i64; 4]> = f
        .m
        .components_plan(level, ddc_model::Lod::Medium)
        .unwrap()
        .into_iter()
        .filter_map(|p| match p {
            ddc_model::ViewPrim::Edge { a, b, .. } => Some(key([a.0, a.1, 0.0], [b.0, b.1, 0.0])),
            _ => None,
        })
        .collect();
    got.sort();
    want.sort();
    assert_eq!(got, want);
}

/// Segment at 1e-6 m, ends in a fixed order.
fn key(a: [f64; 3], b: [f64; 3]) -> [i64; 4] {
    let q = |v: f64| (v * 1e6).round() as i64;
    let (p, r) = ((q(a[0]), q(a[1])), (q(b[0]), q(b[1])));
    let (p, r) = if p <= r { (p, r) } else { (r, p) };
    [p.0, p.1, r.0, r.1]
}

#[test]
fn drawing_units_scale_and_names_never_clash() {
    let f = fx();
    let mut doc = DxfDocument::default();
    doc.header.push(HeaderVar { name: "$INSUNITS".into(), values: vec![Pair::new(70, "4")] });
    doc.blocks.push(ddc_io::dxf::Block::new("Table - 1600x800"));
    append_components(&mut doc, &f.m).unwrap();
    let ins = inserts(&doc);
    let a = ins.iter().find(|i| attr(i, "ID") == f.t1.to_string()).unwrap();
    assert_eq!(a.block, "Table - 1600x800~2");
    assert!((a.pos[0] - 1000.0).abs() < 1e-6 && (a.pos[1] - 1000.0).abs() < 1e-6);
    assert_eq!(attr_tag("Leg H/2"), "LEG_H_2");
}

const STOOL: &str = "family Stool
category furniture
host floor
ifc IfcFurniture CHAIR
param Size length instance 400mm
type \"Std\"
extrude Seat rect(-Size / 2, -Size / 2, Size / 2, Size / 2) z(0m, 450mm)
symbol rect(-Size / 2, -Size / 2, Size / 2, Size / 2)
";

#[test]
fn instance_values_that_change_the_symbol_get_their_own_block() {
    let mut m = Model::open(":memory:").unwrap();
    let level = m.add_level("L1", 0.0).unwrap();
    let fam = m.load_family(STOOL).unwrap();
    let t = m.comp_types(fam).unwrap()[0].0;
    m.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
    m.add_component(&CompSpec::at(level, t, 2.0, 0.0)).unwrap();
    let mut s = CompSpec::at(level, t, 4.0, 0.0);
    s.values = vec![("Size".into(), ParamValue::Num(0.6))];
    m.add_component(&s).unwrap();
    let mut doc = DxfDocument::default();
    let rep = append_components(&mut doc, &m).unwrap();
    assert_eq!((rep.blocks, rep.inserts), (2, 3));
    let names: Vec<&str> = doc.blocks.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["Stool - Std", "Stool - Std v2"]);
    let big = inserts(&doc).into_iter().find(|i| attr(i, "SIZE") == "0.6").unwrap();
    assert_eq!(big.block, "Stool - Std v2");
}
