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
