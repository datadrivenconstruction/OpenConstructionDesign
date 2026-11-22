// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Materials end to end: the README briefs and the hard projects (tower, hospital with its air
//! systems, data centre, plant, lattice tower, free-form museum) drafted through the chat and
//! applied leave no element without a material look and no run without its service colour, the 3D
//! scene carries those looks, and `set_material` changes a look in one undo step.

use crate::e2e_ai_common;

use ddc_app::editor::Editor;
use e2e_ai_common::*;
use serde_json::{json, Value as J};
use std::collections::BTreeSet;

fn editor() -> Editor {
    let mut ed = Editor::new_in_memory().unwrap();
    ed.reload().unwrap();
    ed
}

/// Drafts the brief with `generate_project`, applies it and lists the elements without a look.
fn gaps_of(spec: J) -> Vec<String> {
    let mut ed = editor();
    let (mut s, provider) = run(&mut ed, vec![tool("generate_project", json!({"spec": spec}))]);
    let res = tool_results(&provider);
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let uses = ed.model.material_uses().unwrap();
    assert!(uses.len() > 50, "the brief built too little: {}", uses.len());
    let gaps = ed.model.material_gaps().unwrap();
    gaps.iter().map(|g| format!("{}:{} '{}'", g.table, g.id, g.material)).collect()
}

#[test]
fn the_garden_house_brief_leaves_no_element_without_a_look() {
    let spec = json!({"typology": "house", "name": "Garden House", "floors": 2, "floor_height": 3.0,
        "rooms": [{"kind": "living", "area": 32}, {"kind": "dining", "area": 14},
        {"kind": "kitchen", "area": 14}, {"kind": "bedroom", "area": 15, "count": 3},
        {"kind": "bath", "area": 6, "count": 2}, {"kind": "wc", "area": 2.5}, {"kind": "storage", "area": 4}],
        "roof": {"kind": "hip", "slope": 30, "overhang": 0.6}, "origin": [14, 10],
        "site": {"plot": [[0, 0], [40, 0], [40, 30], [0, 30]], "terrain": {"slope": 0.06, "downhill": [0, 1]}},
        "design": {"terrace": true, "trees": 8}});
    let gaps = gaps_of(spec);
    assert!(gaps.is_empty(), "{} without a look: {gaps:?}", gaps.len());
}

#[test]
fn the_apartment_section_brief_leaves_no_element_without_a_look() {
    let gaps = gaps_of(json!({"typology": "apartment_section", "floors": 5}));
    assert!(gaps.is_empty(), "{} without a look: {gaps:?}", gaps.len());
}

#[test]
fn set_material_gives_a_material_its_look_in_one_undo_step_and_a_repeat_is_no_effect() {
    let mut ed = editor();
    let args = json!({"title": "Steel", "name": "steel S355", "look": "steel", "color": "#7a8088",
        "texture": "metal", "tile": 2.0});
    let (mut s, provider) = run(&mut ed, vec![tool("set_material", args.clone())]);
    let res = tool_results(&provider);
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    assert_eq!(ed.model.material("steel S355").unwrap().id, 0, "a draft never touches the model");
    apply_and_check(&mut s, &mut ed);
    drop(s);
    let m = ed.model.material("steel S355").unwrap();
    assert_eq!(m.spec.color, Some(0x7a8088));
    let r = ddc_model::material_look::resolve("steel S355", m.spec.color).unwrap();
    assert_eq!((r.look.key, r.rgb), ("steel", 0x7a8088));
    let slot = ed.model.material_textures().unwrap();
    assert!(slot.iter().any(|t| t.material == "steel S355"), "{slot:?}");

    let (_s, provider) = run(&mut ed, vec![tool("set_material", json!({"title": "Again", "name": "steel S355", "color": "#7a8088"}))]);
    let res = tool_results(&provider);
    assert!(res.iter().any(|r| r.1 && r.0.contains("no_effect")), "{res:?}");

    let (_s, provider) = run(&mut ed, vec![tool("set_material", json!({"title": "Odd", "name": "x", "look": "marble"}))]);
    let res = tool_results(&provider);
    assert!(res.iter().any(|r| r.1 && r.0.contains("not in the library")), "{res:?}");
}

#[test]
fn list_materials_shows_the_library_and_each_material_look() {
    let mut ed = editor();
    let (_s, provider) = run(&mut ed, vec![tool("list_materials", json!({}))]);
    let res = tool_results(&provider);
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    let text = &res[0].0;
    for w in ["library", "\"steel\"", "\"asphalt\"", "\"glass\"", "elements_without_look"] {
        assert!(text.contains(w), "{w}: {text}");
    }
}

/// The calls drafted and applied in one go.
fn built(calls: Vec<ddc_ai::fake::Script>) -> Editor {
    built_in(editor(), calls)
}

fn built_in(mut ed: Editor, calls: Vec<ddc_ai::fake::Script>) -> Editor {
    let (mut s, provider) = run(&mut ed, calls);
    let res = tool_results(&provider);
    assert!(res.iter().all(|r| !r.1), "{res:?}");
    apply_and_check(&mut s, &mut ed);
    drop(s);
    ed
}

/// No element without a look, no run of a grey service, at least `classes` look classes in use
/// (one class everywhere would be the default tone again), and site surfaces are not concrete.
fn assert_looks(ed: &Editor, classes: usize) -> BTreeSet<&'static str> {
    let uses = ed.model.material_uses().unwrap();
    assert!(uses.len() > 20, "the project built too little: {}", uses.len());
    let gaps = ed.model.material_gaps().unwrap();
    let shown: Vec<String> = gaps.iter().take(10).map(|g| format!("{}:{} '{}'", g.table, g.id, g.material)).collect();
    assert!(gaps.is_empty(), "{} without a look: {shown:?}", gaps.len());
    let mut keys = BTreeSet::new();
    for (table, id, material) in &uses {
        let look = ed.model.material_look(material).unwrap().expect("no gaps, so a look").look.key;
        if matches!(*table, "toposolid" | "topo_pad") {
            assert_ne!(look, "concrete", "{table}:{id} '{material}'");
        }
        keys.insert(look);
    }
    assert!(keys.len() >= classes, "only {keys:?}");
    keys
}

/// Colours of the systems that have runs.
fn service_colours(ed: &Editor) -> BTreeSet<[u8; 3]> {
    let systems = ed.model.mep_systems().unwrap();
    let mut st = ed.model.sql().prepare("SELECT DISTINCT system FROM mep_run").unwrap();
    let used: Vec<i64> = st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    used.iter().filter_map(|id| systems.iter().find(|s| s.id == *id)).map(|s| s.color).collect()
}

/// What the 3D view is handed: run meshes in their service colour (every service with runs shows
/// up), element faces with the texture layer of their look.
fn assert_scene_looks(ed: &mut Editor) {
    use ddc_app::scene_ids::{COMP_BASE, MEP_BASE};
    use ddc_render::scene::TAG_RGB;
    use ddc_render::texture::{layer_of, takes_layer};
    let services = service_colours(ed);
    let (sc, _) = ddc_app::edit3d::scene3d::compose(ed);
    let (mut layered, mut plain) = (0usize, 0usize);
    let (mut layers, mut coloured) = (BTreeSet::new(), BTreeSet::new());
    for m in &sc.meshes {
        if m.tag & TAG_RGB != 0 {
            let c = m.tag & 0xFF_FFFF;
            coloured.insert([(c >> 16) as u8, (c >> 8) as u8, c as u8]);
        }
        if (MEP_BASE..COMP_BASE).contains(&m.element) {
            assert!(m.tag & TAG_RGB != 0, "a run mesh without its colour: {:#x}", m.tag);
            assert!(!ddc_model::material_look::is_neutral(m.tag & 0xFF_FFFF), "a grey run: {:#x}", m.tag);
        } else if let Some(l) = layer_of(m.tag) {
            layers.insert(l);
            layered += 1;
        } else if takes_layer(m.tag) {
            plain += 1;
        }
    }
    assert!(!services.is_empty());
    for c in &services {
        assert!(coloured.contains(c), "service colour {c:?} is not in the scene: {coloured:?}");
    }
    assert!(layers.len() >= 3, "looks in the scene: {layers:?}");
    // wall finishes keep their own colour without a layer, so plain faces are not all grey
    assert!(layered >= 10, "{layered} faces with a look, {plain} without");
}

fn ventilated_hospital() -> Editor {
    built(hospital_calls())
}

/// The H02 setup: an editor that was not reloaded before the brief.
fn ventilated_hospital_h02() -> Editor {
    built_in(Editor::new_in_memory().unwrap(), hospital_calls())
}

fn hospital_calls() -> Vec<ddc_ai::fake::Script> {
    let mut calls = vec![
        tool("generate_project", json!({"spec": {"typology": "hospital_wing", "floors": 4}})),
        tool("load_tools", json!({"groups": ["mep"]})),
    ];
    for kind in ["supply_air", "return_air", "exhaust_air"] {
        let args = json!({"title": kind, "why": "ventilation", "kind": kind, "from": {"riser": [1.0, 1.0]}, "friction_pa_m": 1.0});
        calls.push(tool("route_mep", args));
    }
    calls
}

#[test]
fn the_h02_hospital_shows_its_looks_and_three_air_services_apart_in_the_scene() {
    let mut ed = ventilated_hospital_h02();
    assert_looks(&ed, 3);
    assert!(service_colours(&ed).len() >= 3);
    assert_scene_looks(&mut ed);
}

#[test]
fn a_ventilated_hospital_shows_its_looks_and_three_air_services_apart_in_the_scene() {
    let mut ed = ventilated_hospital();
    assert_looks(&ed, 3);
    let colours = service_colours(&ed);
    assert!(colours.len() >= 3, "supply, return and exhaust read apart: {colours:?}");
    assert_scene_looks(&mut ed);
}

#[test]
fn a_residential_tower_has_a_look_on_every_element() {
    let ed = built(vec![tool("generate_project", json!({"spec": {"typology": "residential_tower", "floors": 12}}))]);
    assert_looks(&ed, 3);
}

#[test]
fn a_curtain_walled_office_tower_has_a_look_on_every_element() {
    // the H05 brief at a third of its height
    let spec = json!({
        "typology": "office", "floors": 12,
        "footprint": {"polygon": [[0, 0], [45, 0], [45, 45], [0, 45]]},
        "lifts": 4, "sprinklers": true, "design": {"facade": "curtain"},
        "features": ["curtain wall", "sprinklers"]
    });
    let ed = built(vec![tool("generate_project", json!({"spec": spec}))]);
    assert_looks(&ed, 3);
}

#[test]
fn a_data_centre_has_a_look_on_every_element_and_every_service() {
    let spec = json!({
        "typology": "office", "floors": 1, "floor_height": 5.0, "band_depth": 24.0,
        "footprint": {"width": 72.0},
        "rooms": [
            {"name": "Data hall", "kind": "technical", "area": 720.0, "count": 3},
            {"name": "UPS room", "kind": "technical", "area": 150.0, "count": 2},
            {"name": "Battery room", "kind": "technical", "area": 100.0, "count": 2},
            {"name": "Chiller plant", "kind": "technical", "area": 120.0}
        ],
        "rules": {"dc.racks": 600}
    });
    let ed = built(vec![tool("generate_project", json!({"spec": spec}))]);
    assert_looks(&ed, 3);
    assert!(!service_colours(&ed).is_empty(), "the data centre has its services");
}

#[test]
fn a_boiler_plant_has_a_look_on_every_element_and_every_line() {
    let ed = built(vec![tool("design_plant", json!({"why": "plant", "brief": "Boiler house 2 MW with 3 boilers"}))]);
    assert_looks(&ed, 2);
    assert!(!service_colours(&ed).is_empty(), "the plant has its lines");
}

#[test]
fn a_lattice_tower_has_a_look_on_every_member() {
    let ed = built(vec![tool(
        "run_generator",
        json!({"id": "steel_frame", "params": {"kind": "lattice_tower_300"}, "variants": 1}),
    )]);
    let keys = assert_looks(&ed, 1);
    assert!(keys.contains("steel"), "{keys:?}");
}

#[test]
fn a_free_form_museum_has_a_look_on_every_element() {
    let spec = json!({
        "typology": "office", "floors": 3, "lang": "en", "name": "Museum",
        "footprint": {"width": 70.0, "depth": 40.0},
        "features": ["sculptural", "wave", "double_curvature", "curved_roof"],
        "ground_rooms": [{"kind": "atrium", "area": 450, "name": "Atrium"}, {"kind": "gallery", "area": 220, "count": 5, "name": "Gallery"}],
        "rooms": [{"kind": "gallery", "area": 260, "count": 4, "name": "Gallery"}],
        "design": {"shell": true}
    });
    let ed = built(vec![tool("generate_project", json!({"spec": spec}))]);
    assert_looks(&ed, 3);
}

#[test]
fn a_free_form_museum_with_its_whole_programme_has_a_look_on_every_element() {
    let spec = json!({
        "typology": "office", "floors": 3, "lang": "en", "name": "Museum",
        "footprint": {"width": 70.0, "depth": 40.0},
        "features": ["sculptural", "wave", "double_curvature", "curved_roof"],
        "ground_rooms": [
            {"kind": "atrium", "area": 450, "name": "Atrium"},
            {"kind": "foyer", "area": 150, "name": "Foyer"},
            {"kind": "gallery", "area": 220, "count": 5, "name": "Gallery"},
            {"kind": "cafe", "area": 120, "name": "Cafe"},
            {"kind": "retail", "area": 80, "name": "Shop"},
            {"kind": "office", "area": 40, "count": 2, "name": "Office"},
            {"kind": "storage", "area": 60, "name": "Storage"}
        ],
        "rooms": [
            {"kind": "gallery", "area": 260, "count": 4, "name": "Gallery"},
            {"kind": "office", "area": 40, "count": 3, "name": "Office"},
            {"kind": "storage", "area": 60, "name": "Storage"}
        ],
        "design": {"shell": true}
    });
    let ed = built(vec![tool("generate_project", json!({"spec": spec}))]);
    assert_looks(&ed, 3);
}

#[test]
fn the_check_finds_an_unknown_material_and_a_run_of_a_grey_system() {
    let ed = ventilated_hospital();
    assert!(ed.model.material_gaps().unwrap().is_empty());
    let sys: i64 = ed.model.sql().query_row("SELECT system FROM mep_run LIMIT 1", [], |r| r.get(0)).unwrap();
    let wtype: i64 = ed.model.sql().query_row("SELECT wtype FROM wall LIMIT 1", [], |r| r.get(0)).unwrap();
    ed.model.sql().execute("UPDATE mep_system SET color=8421504 WHERE id=?1", [sys]).unwrap();
    ed.model.sql().execute("UPDATE wall_type_layer SET material='XYZ' WHERE wtype=?1", [wtype]).unwrap();
    let gaps = ed.model.material_gaps().unwrap();
    assert!(gaps.iter().any(|g| g.table == "mep_run"), "{gaps:?}");
    assert!(gaps.iter().any(|g| g.table == "wall" && g.material == "XYZ"), "{gaps:?}");
}
