// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plants in the 3D scene: the realistic style builds them at fine detail, and trees of one type
//! at different sizes are one shared mesh with the size in each placement.

use ddc_app::editor::Editor;
use ddc_app::mep3d::scene;
use ddc_model::{CompSpec, ParamValue};
use ddc_render::Scene;

#[test]
fn realistic_plants_are_fine_and_one_mesh_per_type() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let f = ed.model.load_library_family("Tree").unwrap();
    let t = ed.model.comp_type_id(f, "Oak A").unwrap().unwrap();
    for k in 0..5 {
        let mut c = CompSpec::at(level, t, 15.0 * k as f64, 0.0);
        c.values = vec![("Scale".into(), ParamValue::Num(0.8 + 0.1 * k as f64))];
        ed.model.add_component(&c).unwrap();
    }
    // plant groups: component keys carry the family above bit 40
    let tris = |sc: &Scene| -> (usize, usize) {
        let g: Vec<_> = sc.instanced.iter().filter(|g| g.key >> 40 != 0).collect();
        (
            g.iter().map(|g| g.proto.triangles.len()).sum(),
            g.iter().map(|g| g.inst.len()).max().unwrap_or(0),
        )
    };
    ed.big.plants_fine = false;
    let (medium, n) = tris(&scene(&mut ed));
    assert_eq!(n, 5, "five placements of one shared mesh");
    ed.big.plants_fine = true;
    let (fine, n) = tris(&scene(&mut ed));
    assert_eq!(n, 5);
    assert!(fine > medium, "fine {fine} medium {medium}");
    let sizes: Vec<f32> = scene(&mut ed)
        .instanced
        .iter()
        .flat_map(|g| g.inst.iter().map(|i| i.m.x_axis.length()))
        .collect();
    assert!(
        sizes.iter().any(|s| (s - 0.8).abs() < 1e-4)
            && sizes.iter().any(|s| (s - 1.2).abs() < 1e-4),
        "{sizes:?}"
    );
}

/// Leaves are tagged as foliage (their own lighting in the realistic style), the trunk is not.
#[test]
fn leaves_are_foliage_and_wood_is_not() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let f = ed.model.load_library_family("Tree").unwrap();
    let t = ed.model.comp_type_id(f, "Linden A").unwrap().unwrap();
    ed.model.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
    ed.big.plants_fine = true;
    let sc = scene(&mut ed);
    let tags: Vec<u32> = sc.instanced.iter().filter(|g| g.key >> 40 != 0).map(|g| g.proto.tag).collect();
    assert!(tags.iter().any(|t| ddc_render::is_foliage(*t)), "{tags:x?}");
    assert!(tags.iter().any(|t| !ddc_render::is_foliage(*t)), "the trunk keeps a plain colour: {tags:x?}");
    assert!(ddc_app::mep3d::is_foliage_material("Foliage Green") && !ddc_app::mep3d::is_foliage_material("Bark Brown"));
}

/// A tree's ladder ends in a far picture of a few cards; its levels are chosen by the size on
/// screen, and the tone skins, which the coarse levels leave out, keep their place in the ladder.
#[test]
fn a_tree_ends_in_a_far_picture_and_its_levels_follow_the_screen_size() {
    let mut ed = Editor::new_in_memory().unwrap();
    let level = ed.level;
    let f = ed.model.load_library_family("Tree").unwrap();
    let t = ed.model.comp_type_id(f, "Oak A").unwrap().unwrap();
    ed.model.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
    ed.big.plants_fine = true;
    let sc = scene(&mut ed);
    let groups: Vec<_> = sc.instanced.iter().filter(|g| g.key >> 40 != 0).collect();
    assert!(groups.len() >= 4, "wood, leaves and the tone skins: {}", groups.len());
    let mut far_tris = vec![];
    for g in &groups {
        assert_eq!(g.lods.len(), 3, "medium, coarse, far picture");
        assert_eq!(g.screen.len(), 4);
        assert!(g.screen.windows(2).all(|w| w[0] > w[1]) && g.screen[3] == 0.0, "{:?}", g.screen);
        assert!(g.lods.windows(2).all(|w| w[0].1 < w[1].1), "errors grow down the ladder");
        far_tris.push((g.proto.triangles.len(), g.lods[1].0.triangles.len(), g.lods[2].0.triangles.len()));
    }
    // the leaf group: far picture is the lightest real level; wood has one too
    let leaves = far_tris.iter().max_by_key(|t| t.0).unwrap();
    assert!(leaves.2 > 1 && leaves.2 < leaves.1 && leaves.1 < leaves.0, "{far_tris:?}");
    // a skin the coarse level omits is a single degenerate triangle there and in the far picture
    assert!(far_tris.iter().any(|t| t.1 == 1 && t.2 == 1), "{far_tris:?}");
    assert!(far_tris.iter().map(|t| t.2).sum::<usize>() < 130, "{far_tris:?}");
}

/// The crown of a near tree is leaf cards cut out by a picture of leaves (or needles), in its
/// shade, leaf and light tones; the coarse level and the far picture stay solid.
#[test]
fn near_crowns_are_leaf_cards_and_far_ones_solid() {
    use ddc_render::chunks::{leaf_cards, tri_vertices};
    for (ty, needle) in [("Oak A", false), ("Spruce A", true)] {
        let mut ed = Editor::new_in_memory().unwrap();
        let level = ed.level;
        let f = ed.model.load_library_family("Tree").unwrap();
        let t = ed.model.comp_type_id(f, ty).unwrap().unwrap();
        ed.model.add_component(&CompSpec::at(level, t, 0.0, 0.0)).unwrap();
        ed.big.plants_fine = true;
        let sc = scene(&mut ed);
        let mut cards = 0;
        for g in sc.instanced.iter().filter(|g| g.key >> 40 != 0 && ddc_render::is_foliage(g.proto.tag)) {
            let Some(q) = leaf_cards(&g.proto) else { continue };
            cards += 1;
            assert!(q.iter().all(|c| c.1 == needle), "{ty}: needle sprays on conifers only");
            assert!(leaf_cards(&g.lods[0].0).is_some(), "{ty}: medium is cards too");
            assert!(g.lods[1..].iter().all(|l| leaf_cards(&l.0).is_none()), "{ty}: coarse and far stay solid");
            let mut v = vec![];
            tri_vertices(&g.proto, &mut v);
            assert!(v.iter().all(|x| ddc_render::is_card(x.tag)), "{ty}: every corner is cut out by the picture");
            let corners: std::collections::BTreeSet<u32> = v.iter().map(|x| x.tag & 6).collect();
            assert_eq!(corners.len(), 4, "{ty}: the four corners of the picture");
        }
        assert!(cards >= 2, "{ty}: leaf and light tones are cards, {cards}");
    }
}
