// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Library looks as texture layers: an element whose material has no slot still gets the layer
//! of its look, a colour edit repaints, and the scene carries the layer on the face tag.

use super::*;
use crate::edit3d::scene3d;

fn column(ed: &mut Editor, material: &str) -> i64 {
    let shape = ddc_geom::member::catalog().into_iter().find(|c| c.0 == "R 400x400").unwrap().1;
    let mt = ed.model.add_member_type(&format!("C {material}"), &shape, material).unwrap();
    let id = ed
        .model
        .add_column(&ddc_model::ColumnSpec {
            level: ed.level,
            top_level: None,
            member_type: mt,
            structural: true,
            at: (1.0, 1.0),
            base_offset: 0.0,
            top_offset: 3.0,
            rotation: 0.0,
            top_at: None,
        })
        .unwrap();
    ed.reload().unwrap();
    id
}

fn look_key(ed: &Editor, table: &'static str, id: i64) -> Option<String> {
    let l = layers(ed);
    let i = *l.elem.get(&(table, id))? as usize;
    CACHE.with(|c| c.borrow().as_ref().map(|c| c.all_keys[i].clone()))
}

#[test]
fn an_element_without_a_slot_takes_the_layer_of_its_look() {
    let mut ed = Editor::new_in_memory().unwrap();
    let steel = column(&mut ed, "steel S355");
    let timber = column(&mut ed, "glulam GL24h");
    let unknown = column(&mut ed, "XYZ");
    let k = look_key(&ed, "col", steel).expect("steel column has a layer");
    assert!(k.starts_with("look|metal|"), "{k}");
    assert!(look_key(&ed, "col", timber).unwrap().starts_with("look|wood|"));
    assert_eq!(look_key(&ed, "col", unknown), None, "no look, no layer");
    // two names of one class share one layer
    let other = column(&mut ed, "S355");
    let l = layers(&ed);
    assert_eq!(l.elem[&("col", steel)], l.elem[&("col", other)]);
}

#[test]
fn a_colour_edit_gives_the_material_a_new_layer() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = column(&mut ed, "Custom steel");
    let before = look_key(&ed, "col", id).unwrap();
    let mut spec = ddc_model::material_defaults("Custom steel");
    spec.color = Some(0x2F5C9E);
    // the member type already wrote the row of its material text
    ed.model.set_material("Custom steel", &spec).unwrap();
    ed.reload().unwrap();
    let after = look_key(&ed, "col", id).unwrap();
    assert_ne!(before, after);
    assert!(after.contains("|2f5c9e|"), "{after}");
}

#[test]
fn the_scene_tag_carries_the_look_layer_and_keeps_the_category() {
    let mut ed = Editor::new_in_memory().unwrap();
    let id = column(&mut ed, "concrete C30/37");
    let (sc, ts) = scene3d::compose(&mut ed);
    let k = ts
        .iter()
        .position(|t| *t == crate::edit3d::pick::Target::new(crate::edit3d::pick::Cat::Column, id))
        .unwrap();
    let m = sc
        .meshes
        .iter()
        .find(|m| m.element == crate::scene_ids::EXT_BASE + k as u32)
        .unwrap();
    assert!(texture::layer_of(m.tag).is_some(), "{:#x}", m.tag);
    assert_eq!(ddc_render::style::base_tag(m.tag), scene3d::TAG_MEMBER);
}

#[test]
fn a_part_takes_the_look_of_its_material() {
    use ddc_geom::{arc::BulgeRing, v2};
    use ddc_model::{SolidForm, SolidFrame, SolidSpec};
    let mut ed = Editor::new_in_memory().unwrap();
    let ring = |r: f64| BulgeRing {
        pts: (0..8).map(|i| (std::f64::consts::TAU * i as f64 / 8.0, r)).map(|(a, r)| v2(r * a.cos(), r * a.sin())).collect(),
        bulges: vec![0.0; 8],
    };
    let mut add = |material: &str, top: f64| {
        ed.model
            .add_solid(&SolidSpec {
                level: ed.level,
                name: "p".into(),
                void: false,
                material: material.into(),
                frame: SolidFrame::default(),
                form: SolidForm::Blend { bottom: ring(1.0), top: ring(top), height: 1.0 },
                ifc: None,
            })
            .unwrap()
    };
    let (steel, grass) = (add("steel", 0.8), add("grass", 0.5));
    ed.reload().unwrap();
    let (sc, ts) = scene3d::compose(&mut ed);
    let tag_of = |id: i64| {
        let k = ts.iter().position(|t| *t == crate::edit3d::pick::Target::new(crate::edit3d::pick::Cat::Ext(crate::scene_ids::EXT_SOLID), id)).unwrap();
        sc.meshes.iter().find(|m| m.element == crate::scene_ids::EXT_BASE + k as u32).unwrap().tag
    };
    assert!(texture::layer_of(tag_of(steel)).is_some(), "{:#x}", tag_of(steel));
    assert_eq!(tag_of(grass) & ddc_render::scene::TAG_RGB, ddc_render::scene::TAG_RGB);
}
