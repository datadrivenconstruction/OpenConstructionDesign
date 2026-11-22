// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Rules of `material_look`: every library class is reachable from its own name, every name
//! scripts and generators write finds its class, colours and services, and the gap check.

use ddc_geom::mep::SystemKind;
use ddc_ops::{Actor, Cat, Dir3, Envelope, Form, Frame, GeoOp, Group, Len, Op, Profile, Ref, P3};

use crate::material_look::*;
use crate::{Model, BUILTIN_TEXTURES};

fn key(name: &str) -> Option<&'static str> {
    look_of(name).map(|l| l.key)
}

#[test]
fn every_class_is_found_by_its_own_name_and_has_a_built_in_texture() {
    for l in LOOKS.iter() {
        assert_eq!(key(l.name), Some(l.key), "{}", l.name);
        assert!(BUILTIN_TEXTURES.contains(&l.texture), "{} texture {}", l.key, l.texture);
        assert!(l.tile_m > 0.0 && (0.0..=1.0).contains(&l.roughness) && (0.0..=1.0).contains(&l.metallic));
        assert!(l.rgb <= 0xFF_FF_FF);
    }
    let mut keys: Vec<_> = LOOKS.iter().map(|l| l.key).collect();
    keys.sort_unstable();
    keys.dedup();
    assert_eq!(keys.len(), LOOKS.len(), "keys are unique");
}

#[test]
fn names_scripts_and_generators_write_find_their_class() {
    let cases = [
        ("steel S355", "steel"),
        ("S355", "steel"),
        ("S235", "steel"),
        ("steel S350GD", "steel"),
        ("steel galvanised", "steel"),
        ("steel strand", "steel"),
        ("steel grating", "steel"),
        ("stainless steel", "steel"),
        ("finned tubes", "steel"),
        ("Steel", "steel"),
        ("cast iron", "iron"),
        ("concrete C40/50", "concrete"),
        ("C30/37", "concrete"),
        ("precast concrete", "concrete"),
        ("Reinforced Concrete", "concrete"),
        ("Cement Screed", "concrete"),
        ("Lightweight Concrete To Falls", "concrete"),
        ("concrete paving", "paving"),
        ("glulam GL24h", "timber"),
        ("GL28h", "timber"),
        ("C24", "timber"),
        ("timber decking", "timber"),
        ("oak", "timber"),
        ("Cross-laminated timber", "timber"),
        ("glass", "glass"),
        ("red glass", "glass"),
        ("polycarbonate", "polycarbonate"),
        ("aluminium", "aluminium"),
        ("perforated aluminium", "aluminium"),
        ("aluminium composite", "cladding"),
        ("metal cladding", "cladding"),
        ("Brick", "brick"),
        ("Calcium Silicate Masonry", "masonry"),
        ("Mineral Render", "render"),
        ("Gypsum Plaster", "plaster"),
        ("Moisture Resistant Gypsum Board", "plaster"),
        ("Mineral Wool Facade Insulation", "insulation"),
        ("XPS Insulation", "insulation"),
        ("Bitumen Roofing Membrane", "roofing"),
        ("Clay tile", "roof_tile"),
        ("asphalt", "asphalt"),
        ("road paint white", "paint"),
        ("line paint", "paint"),
        ("grass pitch", "grass"),
        ("Lawn Grass", "grass"),
        ("soil", "soil"),
        ("Planting Soil", "soil"),
        ("water", "water"),
        ("PTFE fabric", "fabric"),
        ("elastomer steel", "rubber"),
        ("led panel", "light"),
        ("Foliage Green", "foliage"),
        ("Bark Brown", "bark"),
        ("stadium seating", "seating"),
        ("sandstone", "stone"),
        ("weathering steel", "steel"),
    ];
    for (name, want) in cases {
        assert_eq!(key(name), Some(want), "{name}");
    }
    // whole words only for short words: these must not catch a class by a fragment
    assert_eq!(key("pitched"), None);
    assert_eq!(key("sandwich panel"), Some("cladding"));
    for none in ["", "  ", "XYZ", "Material 1", "f1 D24"] {
        assert_eq!(key(none), None, "{none:?}");
    }
}

#[test]
fn colour_words_and_rows_set_the_colour() {
    let r = resolve("road paint white", None).unwrap();
    assert_eq!(r.rgb, 0xF0F0EC);
    // a painted metal is not metallic
    let w = resolve("Steel White", None).unwrap();
    assert_eq!((w.look.key, w.rgb, w.metallic), ("steel", 0xF0F0EC, 0.0));
    assert!(resolve("steel S355", None).unwrap().metallic > 0.5);
    // a row colour wins over the class and the name
    assert_eq!(resolve("concrete red", Some(0x123456)).unwrap().rgb, 0x123456);
    // an unknown name with a colour is a coat of paint
    assert_eq!(resolve("Custom", Some(0xABCDEF)).map(|r| (r.look.key, r.rgb)), Some(("paint", 0xABCDEF)));
    assert_eq!(resolve("Custom", None), None);
}

#[test]
fn defaults_by_kind_and_by_name_hint_resolve_to_their_class() {
    assert_eq!(default_material("column", ""), "Steel");
    assert_eq!(default_material("brace", ""), "Steel");
    assert_eq!(default_material("roof", ""), "Roofing");
    assert_eq!(default_material("curtain", ""), "Glass");
    assert_eq!(default_material("mullion", "Curtain system"), "Aluminium");
    assert_eq!(default_material("toposolid", ""), "Soil");
    assert_eq!(default_material("solid", ""), "Concrete");
    assert_eq!(default_material("solid", "f1 D24"), "Concrete");
    assert_eq!(default_material("solid", "Seat row 3"), "Seating");
    assert_eq!(default_material("solid", "Pitch grass"), "Grass");
    for kind in ["wall", "slab", "column", "beam", "roof", "curtain", "mullion", "panel", "ground", "duct", "solid"] {
        let m = default_material(kind, "");
        assert!(look_of(m).is_some(), "{kind}: {m} has a look");
    }
}

#[test]
fn services_take_their_colour_and_fuel_gas_its_own() {
    assert_eq!(service_rgb("S1", SystemKind::SupplyAir), SystemKind::SupplyAir.color());
    assert_eq!(service_rgb("P-1 NG 50-1-A1", SystemKind::Heating), GAS_RGB);
    assert_eq!(service_rgb("Gas supply", SystemKind::Heating), GAS_RGB);
    assert_eq!(service_rgb("Medical gas O2", SystemKind::MedicalGas), SystemKind::MedicalGas.color());
    assert_eq!(service_rgb("P-1 HWS 50-2-A1", SystemKind::Heating), SystemKind::Heating.color());
    for k in SystemKind::ALL {
        assert_ne!(k.color(), GAS_RGB, "gas reads apart from {k:?}");
    }
    let mut m = Model::open(":memory:").unwrap();
    let id = m.add_mep_system("P-1 NG 25-4-A1", SystemKind::Heating).unwrap();
    let s = m.mep_systems().unwrap().into_iter().find(|s| s.id == id).unwrap();
    assert_eq!(s.color, GAS_RGB);
}

fn solid_row(m: &Model, material: &str, void: bool) -> i64 {
    m.db.execute(
        "INSERT INTO solid(level,name,kind,void,material,plane,def) VALUES (1,'s','extrude',?1,?2,'','')",
        rusqlite::params![void, material],
    )
    .unwrap();
    m.db.last_insert_rowid()
}

#[test]
fn gaps_list_empty_and_unknown_materials_but_not_voids_or_coloured_rows() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let empty = solid_row(&m, "", false);
    let unknown = solid_row(&m, "XYZ", false);
    solid_row(&m, "steel S355", false);
    solid_row(&m, "", true);
    let painted = solid_row(&m, "Custom", false);
    let gaps = m.material_gaps().unwrap();
    let ids: Vec<i64> = gaps.iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![empty, unknown, painted]);
    assert!(gaps.iter().all(|g| g.table == "solid"));
    // a row with a colour gives the unknown name a look
    let mut spec = crate::material_defaults("Custom");
    spec.color = Some(0x336699);
    m.add_material(&spec).unwrap();
    let ids: Vec<i64> = m.material_gaps().unwrap().iter().map(|g| g.id).collect();
    assert_eq!(ids, vec![empty, unknown]);
    // the solid's material shows in the texture lookup too
    assert!(m.element_materials().unwrap().iter().any(|e| e.0 == "solid" && e.2 == "steel S355"));
}

fn env(ops: Vec<Op>) -> Envelope {
    let mut e = Envelope::new(Actor::Script { name: "test".into() }, "material test");
    e.groups.push(Group { id: 1, title: "g".into(), ops, depends_on: vec![], why: String::new() });
    e
}

#[test]
fn a_part_lowered_without_a_material_gets_one_from_its_name_or_kind() {
    let mut m = Model::open(":memory:").unwrap();
    let l = Ref::id(Cat::Level, m.add_level("L1", 0.0).unwrap());
    let part = |slot: u32, name: Option<&str>, material: Option<&str>| {
        Op::Geometry(GeoOp::Solid {
            slot,
            level: Some(l.clone()),
            form: Form::Extrude {
                profile: Profile::Rect { w: Len::mm(100.0), h: Len::mm(200.0) },
                frame: Frame { origin: P3::m(slot as f64, 0.0, 0.0), x: Dir3::Y, z: Dir3::X },
                start: Len::m(0.0),
                end: Len::m(1.0),
            },
            material: material.map(str::to_string),
            name: name.map(str::to_string),
            ifc: None,
            role: None,
        })
    };
    m.apply_envelope_step(&env(vec![
        part(0, None, None),
        part(1, Some("Seat row"), None),
        part(2, Some("x"), Some("glulam GL24h")),
        part(3, Some("y"), Some("  ")),
    ]))
    .unwrap();
    let mats: Vec<String> = m
        .db
        .prepare("SELECT material FROM solid ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(mats, ["Concrete", "Seating", "glulam GL24h", "Concrete"]);
    assert!(m.material_gaps().unwrap().is_empty());
}

fn rgb_dist(a: [u8; 3], b: [u8; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (f64::from(*x) - f64::from(y)).powi(2)).sum::<f64>().sqrt()
}

#[test]
fn every_service_has_a_hue_of_its_own_far_from_every_other() {
    let mut all: Vec<(String, [u8; 3])> =
        SystemKind::ALL.iter().map(|k| (k.name().to_string(), service_rgb(k.name(), *k))).collect();
    all.push(("gas".into(), service_rgb("NG supply", SystemKind::Heating)));
    for (i, (a, ca)) in all.iter().enumerate() {
        let packed = u32::from(ca[0]) << 16 | u32::from(ca[1]) << 8 | u32::from(ca[2]);
        assert!(!is_neutral(packed), "{a} is grey: {ca:?}");
        for (b, cb) in &all[i + 1..] {
            let d = rgb_dist(*ca, *cb);
            assert!(d >= 60.0, "{a} {ca:?} and {b} {cb:?} read alike: {d:.0}");
        }
    }
}

#[test]
fn an_element_written_without_a_material_gets_the_one_of_its_kind_or_layer_function() {
    assert_eq!(assign("member", "", "HEA 200"), "Steel");
    assert_eq!(assign("member", "", "Glulam beam"), "Timber");
    assert_eq!(assign("member", "  steel S355 ", ""), "steel S355", "a given name is kept");
    assert_eq!(assign("insulation", "", ""), "Insulation");
    assert_eq!(assign("finish1", "", ""), "Plaster");
    assert_eq!(assign("membrane", "", ""), "Rubber");
    assert_eq!(assign("topo_pad", "", "Lawn"), "Grass");
    assert_eq!(assign("topo_pad", "", "Car park paving"), "Paving");
    for f in ["structure", "substrate", "insulation", "air", "membrane", "finish1", "finish2"] {
        assert!(look_of(&assign(f, "", "")).is_some(), "{f}");
    }
    let mut m = Model::open(":memory:").unwrap();
    let shape = ddc_geom::member::catalog().into_iter().find(|c| c.0 == "R 400x400").unwrap().1;
    let id = m.add_member_type("HEA 200", &shape, "").unwrap();
    let mat: String = m.db.query_row("SELECT material FROM member_type WHERE id=?1", [id], |r| r.get(0)).unwrap();
    assert_eq!(mat, "Steel");
}

fn mep_run_row(m: &Model, system: i64) -> i64 {
    m.db.execute(
        "INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,material,dn,bend,slope,points,cap0,cap1) \
         VALUES (?1,1,'duct','round',0.3,0.3,0.001,0,0,NULL,NULL,NULL,0,'0 0 3;5 0 3',0,0)",
        [system],
    )
    .unwrap();
    m.db.last_insert_rowid()
}

#[test]
fn runs_of_a_grey_or_missing_system_are_gaps_and_service_coloured_ones_are_not() {
    let mut m = Model::open(":memory:").unwrap();
    m.add_level("L1", 0.0).unwrap();
    let supply = m.add_mep_system("SA-1", SystemKind::SupplyAir).unwrap();
    m.db.execute("INSERT INTO mep_system(name,kind,color) VALUES ('Grey',?1,?2)", rusqlite::params!["supply_air", 0x80_80_80]).unwrap();
    let grey = m.db.last_insert_rowid();
    let ok = mep_run_row(&m, supply);
    let dull = mep_run_row(&m, grey);
    let lost = mep_run_row(&m, 9999);
    let gaps: Vec<(&str, i64)> = m.material_gaps().unwrap().iter().map(|g| (g.table, g.id)).collect();
    assert_eq!(gaps, vec![("mep_run", dull), ("mep_run", lost)]);
    assert!(!gaps.contains(&("mep_run", ok)));
}
