use super::saved::{apply_spec, sel_from_key, sel_key, spec_of};
use super::scene::{build, cat_index, fp, tag_category, Deco};
use super::*;
use crate::editor::Editor;
use ddc_render::glam::{Mat4, Vec3};
use ddc_render::style::is_cap;
use ddc_render::{Inst, Instanced, Scene, SceneMesh};
use std::sync::Arc;

fn cube(element: u32, tag: u32, min: [f32; 3], max: [f32; 3]) -> SceneMesh {
    let positions: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { min[0] } else { max[0] },
                if i & 2 == 0 { min[1] } else { max[1] },
                if i & 4 == 0 { min[2] } else { max[2] },
            ]
        })
        .collect();
    let q = |a, b, c, d| [[a, b, c], [a, c, d]];
    let triangles = [
        q(0, 2, 3, 1),
        q(4, 5, 7, 6),
        q(0, 1, 5, 4),
        q(2, 6, 7, 3),
        q(0, 4, 6, 2),
        q(1, 3, 7, 5),
    ]
    .concat();
    SceneMesh {
        element,
        tag,
        positions,
        triangles,
    }
}

fn aabb(min: [f32; 3], max: [f32; 3]) -> Aabb {
    Aabb {
        min: Vec3::from(min),
        max: Vec3::from(max),
    }
}

#[test]
fn category_rules_and_temporary_modes() {
    let mut n = Nav::default();
    assert!(CATS
        .iter()
        .all(|c| n.cat_visible(c) && n.cat_transparency(c) == 0));
    n.rules.insert(
        "floor".into(),
        CatRule {
            visible: false,
            transparency: 0,
        },
    );
    n.rules.insert(
        "wall".into(),
        CatRule {
            visible: true,
            transparency: 40,
        },
    );
    assert!(!n.cat_visible("floor") && n.cat_visible("wall"));
    assert_eq!(n.cat_transparency("wall"), 40);
    n.temp = Some(TempCats::Hide(["door".to_string()].into()));
    assert!(!n.cat_visible("door") && n.cat_visible("window"));
    n.temp = Some(TempCats::Isolate(
        ["wall".to_string(), "floor".to_string()].into(),
    ));
    assert!(n.cat_visible("wall") && !n.cat_visible("door"));
    // the view rule overrides isolation
    assert!(!n.cat_visible("floor"));
    // uniform coverage table: hidden — 0, 40 % transparency — 0.6, outside the rules — 1
    let t = n.coverage_table();
    assert_eq!(t[0], 1.0);
    assert_eq!(t[cat_index("floor") as usize], 0.0);
    assert_eq!(t[cat_index("door") as usize], 0.0);
    assert!((t[cat_index("wall") as usize] - 0.6).abs() < 1e-6);
    n.temp = None;
    assert_eq!(n.coverage_table()[cat_index("door") as usize], 1.0);
}

#[test]
fn style_turns_effects_off() {
    let mut n = Nav::default();
    assert_eq!(n.quality(Quality::High), Quality::High);
    for s in [
        VisualStyle::Consistent,
        VisualStyle::HiddenLine,
        VisualStyle::Wireframe,
    ] {
        n.style = s;
        assert_eq!(n.quality(Quality::High), Quality::Off);
    }
}

#[test]
fn extents_clip_to_section_box() {
    let b = aabb([0.0; 3], [10.0, 10.0, 6.0]);
    let s = aabb([2.0, -5.0, 1.0], [4.0, 20.0, 3.0]);
    let e = Nav::extents(Some(b), Some(s)).unwrap();
    assert_eq!(e, aabb([2.0, 0.0, 1.0], [4.0, 10.0, 3.0]));
    assert_eq!(Nav::extents(Some(b), None), Some(b));
    assert_eq!(Nav::extents(None, Some(s)), Some(s));
    // box outside the model — fit the box itself
    let far = aabb([50.0; 3], [60.0; 3]);
    assert_eq!(Nav::extents(Some(b), Some(far)), Some(far));
}

#[test]
fn level_box_cuts_above_level() {
    let b = aabb([-2.0, -1.0, -6.0], [12.0, 8.0, 9.0]);
    let l = level_box(b, LEVEL_CUT);
    assert!((l.max.z - LEVEL_CUT as f32).abs() < 1e-6);
    assert!((l.min.z + LEVEL_BELOW as f32).abs() < 1e-6);
    assert!(l.min.x < b.min.x && l.max.y > b.max.y);
    assert!(level_box(b, 0.0).max.z > 0.0);
}

#[test]
fn tag_categories() {
    assert_eq!(tag_category(0), "wall");
    assert_eq!(tag_category(ddc_render::TAG_DOOR), "door");
    assert_eq!(tag_category(ddc_render::TAG_WINDOW), "window");
    assert_eq!(tag_category(ddc_render::TAG_PANEL), "curtain");
    assert_eq!(tag_category(ddc_render::TAG_SLAB), "floor");
    assert_eq!(tag_category(0x4000_0000 | 0x12_3456), "mep");
    assert_eq!(tag_category(0x8000_0000), "other");
    // the category number in the tag does not change the category
    let t = ddc_render::renderer::with_cat(ddc_render::TAG_SLAB, 7);
    assert_eq!(tag_category(t), "floor");
    assert!(CATS.iter().all(|c| (1..16).contains(&cat_index(c))));
    assert_eq!(cat_index("nope"), 0);
}

#[test]
fn mep_and_components_have_categories() {
    let ed = Editor::new_in_memory().unwrap();
    assert_eq!(sel_category(&ed, Sel::Mep(1)), "mep");
    assert_eq!(sel_category(&ed, Sel::Comp(1)), "other");
}

#[test]
fn sel_keys_roundtrip() {
    for s in [
        Sel::Wall(3),
        Sel::Opening(4),
        Sel::Curtain(5),
        Sel::Panel(6, 11),
        Sel::Slab(7),
        Sel::Room(8),
        Sel::Mep(9),
        Sel::Comp(10),
    ] {
        let (k, id) = sel_key(s).unwrap();
        assert_eq!(sel_from_key(&k, id), Some(s));
    }
    assert_eq!(sel_from_key("nope", 1), None);
    assert_eq!(sel_from_key("panel.x", 1), None);
}

#[test]
fn saved_view_state_roundtrip_across_levels() {
    let mut cam = OrbitCamera {
        target: Vec3::new(3.0, -2.0, 1.5),
        yaw: 0.7,
        pitch: -0.2,
        distance: 25.0,
        ortho: true,
        ..Default::default()
    };
    cam.fov_y = 0.9;
    let mut n = Nav {
        style: VisualStyle::XRay,
        caps: false,
        level_view: Some((4, 1.5)),
        ..Default::default()
    };
    n.rules.insert(
        "mep".into(),
        CatRule {
            visible: true,
            transparency: 70,
        },
    );
    n.rules.insert("door".into(), CatRule::default()); // default — not saved
    let bx = aabb([-1.0, -1.0, -0.3], [5.0, 4.0, 1.5]);
    let elev = 3.0;
    let s = spec_of(
        "v",
        &cam,
        Some(bx),
        &n,
        [Sel::Wall(9), Sel::Panel(2, 1)],
        elev,
    );
    assert_eq!(s.categories.len(), 1);
    assert!((s.camera.target[2] - 4.5).abs() < 1e-6);
    assert!((s.section_box.unwrap().0[2] - 2.7).abs() < 1e-6);
    // the model stores JSON: the same view after parsing
    let s = ddc_model::View3dSpec::from_json("v", &s.to_json()).unwrap();

    // open at a level with elevation 0: scene z = model elevation
    let (mut c2, mut b2, mut n2) = (OrbitCamera::default(), None, Nav::default());
    let hidden = apply_spec(&s, &mut c2, &mut b2, &mut n2, 0.0);
    assert!((c2.target - Vec3::new(3.0, -2.0, 4.5)).length() < 1e-5);
    assert!((c2.yaw - 0.7).abs() < 1e-6 && (c2.pitch + 0.2).abs() < 1e-6);
    assert!(c2.ortho && (c2.distance - 25.0).abs() < 1e-4 && (c2.fov_y - 0.9).abs() < 1e-6);
    let b2 = b2.unwrap();
    assert!((b2.min.z - 2.7).abs() < 1e-5 && (b2.max.z - 4.5).abs() < 1e-5);
    assert_eq!(n2.style, VisualStyle::XRay);
    assert!(!n2.caps);
    assert_eq!(n2.level_view, Some((4, 1.5)));
    assert_eq!(n2.cat_transparency("mep"), 70);
    assert_eq!(hidden.len(), 2);
    assert!(hidden.contains(&Sel::Panel(2, 1)) && hidden.contains(&Sel::Wall(9)));

    // at the original level — the original scene coordinates
    let (mut c3, mut b3, mut n3) = (OrbitCamera::default(), None, Nav::default());
    apply_spec(&s, &mut c3, &mut b3, &mut n3, elev);
    assert!((c3.target - cam.target).length() < 1e-5);
    let b3 = b3.unwrap();
    assert!((b3.min - bx.min).length() < 1e-5 && (b3.max - bx.max).length() < 1e-5);
}

#[test]
fn a_scene_of_instances_only_reaches_the_view_and_can_be_unloaded() {
    let ed = Editor::new_in_memory().unwrap();
    let group = Instanced::new(
        1,
        Arc::new(cube(u32::MAX, 0, [0.0; 3], [1.0; 3])),
        vec![Inst {
            m: Mat4::IDENTITY,
            element: 5,
        }],
    );
    let full = Scene::shared(vec![]).with_instanced(vec![group]);
    let empty = Scene::shared(vec![]);
    assert_ne!(fp(&full), fp(&empty), "no flat meshes in either");
    assert_ne!(
        fp(&Scene::shared(vec![])),
        fp(&empty),
        "two empty builds are two builds"
    );
    let mut d = Deco::default();
    assert_ne!(
        Some(fp(&Scene::default())),
        d.out_fp,
        "the first frame sets the base"
    );
    d.set_base(&ed, full);
    let sc = d.output(&ed, None, &Default::default(), 0).unwrap();
    assert_eq!(sc.instanced.len(), 1);
    assert_eq!(d.out_fp, Some(fp(&sc)), "its own output is not a new base");
    d.set_base(&ed, empty);
    let sc = d.output(&ed, None, &Default::default(), 0).unwrap();
    assert!(
        sc.instanced.is_empty(),
        "unloading the model empties the view"
    );
}

fn base_scene() -> Scene {
    let wall = cube(0, 0, [0.0, 0.0, 0.0], [4.0, 0.2, 3.0]);
    let slab = cube(1, ddc_render::TAG_SLAB, [0.0, 0.0, -0.2], [4.0, 4.0, 0.0]);
    let col = Instanced::new(
        9,
        Arc::new(cube(u32::MAX, 0, [0.0; 3], [0.3, 0.3, 3.0])),
        vec![
            Inst {
                m: Mat4::from_translation(Vec3::new(1.0, 2.0, 0.0)),
                element: 5,
            },
            Inst {
                m: Mat4::from_translation(Vec3::new(3.0, 2.0, 0.0)),
                element: 6,
            },
        ],
    );
    Scene::new(vec![wall, slab]).with_instanced(vec![col])
}

#[test]
fn build_shares_meshes_keeps_instances_and_categories() {
    let base = base_scene();
    let cats = [cat_index("wall"), cat_index("floor"), cat_index("other")];
    let sc = build(&base, &cats, &|_| false, &[]);
    // the same `Arc`s (GPU chunks and the index recognise the meshes), instances are not expanded
    assert_eq!(sc.meshes.len(), 2);
    assert!(sc
        .meshes
        .iter()
        .zip(&base.meshes)
        .all(|(a, b)| Arc::ptr_eq(a, b)));
    assert_eq!(sc.instanced.len(), 1);
    assert!(Arc::ptr_eq(&sc.instanced[0].inst, &base.instanced[0].inst));
    assert_eq!(sc.bounds, base.bounds);
    assert_eq!(
        (sc.cat(0), sc.cat(1), sc.cat(2)),
        (cats[0], cats[1], cats[2])
    );
    // hidden by the view: the slab and one column; category numbers shift accordingly
    let sc = build(&base, &cats, &|e| e == 1 || e == 5, &[]);
    assert_eq!(sc.meshes.len(), 1);
    assert!(Arc::ptr_eq(&sc.meshes[0], &base.meshes[0]));
    assert_eq!(sc.instanced[0].inst.len(), 1);
    assert_eq!(sc.instanced[0].inst[0].element, 6);
    assert_eq!((sc.cat(0), sc.cat(1)), (cats[0], cats[2]));
    // the fill carries its source category
    let cap = Arc::new(cube(
        0,
        ddc_render::style::cap_tag([1, 2, 3]),
        [0.0; 3],
        [1.0, 1.0, 0.0],
    ));
    let sc = build(&base, &cats, &|_| false, &[(0, cap.clone())]);
    assert!(Arc::ptr_eq(&sc.meshes[2], &cap));
    assert_eq!(sc.cat(2), cats[0]);
    assert_eq!(sc.cat(3), cats[2]);
}

#[test]
fn caps_cached_per_box_and_scene_and_theme() {
    let ed = Editor::new_in_memory().unwrap();
    let mut d = Deco::default();
    d.set_base(&ed, base_scene());
    let poche = [[200, 100, 50]; 7];
    // the box cuts the wall and both columns at height 1.5, but not the slab
    let bx = aabb([-1.0, -1.0, -1.0], [5.0, 5.0, 1.5]);
    let k = d.step_caps(&ed, Some(bx), poche, false);
    assert!(!d.caps_stale);
    let caps: Vec<_> = d.cached_caps().unwrap().to_vec();
    assert_eq!(caps.len(), 3);
    assert!(caps
        .iter()
        .all(|(_, m)| is_cap(m.tag) && m.tag & 0x00FF_FFFF == 0x00C8_6432));
    let wall = caps.iter().find(|(i, _)| *i == 0).unwrap();
    let area: f32 = wall
        .1
        .triangles
        .iter()
        .map(|t| {
            let p = t.map(|i| Vec3::from(wall.1.positions[i as usize]));
            (p[1] - p[0]).cross(p[2] - p[0]).length() * 0.5
        })
        .sum();
    assert!((area - 0.8).abs() < 1e-4, "{area}");
    // columns are in world coordinates, with source `meshes.len()` + group
    let cols: Vec<_> = caps.iter().filter(|(i, _)| *i == 2).collect();
    assert_eq!(cols.len(), 2);
    assert!(cols
        .iter()
        .any(|(_, m)| m.positions.iter().all(|p| p[0] >= 2.9)));
    // the same box — from the cache (same `Arc`s), the view scene is built once
    let sc = d.output(&ed, k, &Default::default(), 0).unwrap();
    assert_eq!(sc.meshes.len(), 5);
    assert!(d.output(&ed, k, &Default::default(), 0).is_none());
    d.step_caps(&ed, Some(bx), poche, false);
    assert!(Arc::ptr_eq(&d.cached_caps().unwrap()[0].1, &caps[0].1));
    assert!(d.output(&ed, k, &Default::default(), 0).is_none());
    // view-hidden elements — new scene; new theme — new fills
    assert!(d.output(&ed, k, &Default::default(), 1).is_some());
    d.step_caps(&ed, Some(bx), [[1, 2, 3]; 7], false);
    let c2 = d.cached_caps().unwrap();
    assert!(c2.iter().all(|(_, m)| m.tag & 0x00FF_FFFF == 0x0001_0203));
    // new base scene — the cache does not match; without a box there are no fills
    d.set_base(&ed, base_scene());
    let k = d.step_caps(&ed, None, poche, false);
    assert!(k.is_none() && !d.caps_stale);
    let sc = d.output(&ed, k, &Default::default(), 1).unwrap();
    assert_eq!(sc.meshes.len(), 2);
}

#[test]
fn pick_skips_hidden_categories_and_cut_away_parts() {
    let base = base_scene();
    let cats = [cat_index("wall"), cat_index("floor"), cat_index("other")];
    let sc = build(&base, &cats, &|_| false, &[]);
    let hit = |mesh: usize, z: f32| ddc_render::Hit {
        mesh,
        triangle: 0,
        element: 0,
        t: 1.0,
        point: Vec3::new(1.0, 0.1, z),
    };
    let mut n = Nav::default();
    let cov = n.coverage_table();
    assert!(pickable(&cov, &sc, None, &hit(0, 1.0)));
    n.rules.insert(
        "wall".into(),
        CatRule {
            visible: true,
            transparency: 80,
        },
    );
    let cov = n.coverage_table();
    assert!(
        !pickable(&cov, &sc, None, &hit(0, 1.0)),
        "almost transparent: picks through"
    );
    assert!(pickable(&cov, &sc, None, &hit(1, 1.0)));
    let bx = Some(aabb([-1.0; 3], [5.0, 5.0, 1.5]));
    assert!(pickable(&cov, &sc, bx, &hit(1, 1.0)));
    assert!(!pickable(&cov, &sc, bx, &hit(1, 2.0)), "cut off by the box");
}

#[test]
fn orbit_pivot_falls_back_to_target() {
    let mut n = Nav::default();
    let mut cam = OrbitCamera::default();
    cam.target = Vec3::new(1.0, 2.0, 3.0);
    cam.distance = 10.0;
    // off the model — around the camera target, not a point on the ground
    n.orbit_drag(&mut cam, egui::vec2(5.0, 0.0), None, None);
    assert_eq!(n.pivot, Some(Vec3::new(1.0, 2.0, 3.0)));
    // a hit far beyond the limit — also the target
    n.pivot = None;
    let far = cam.eye() + (cam.target - cam.eye()).normalize() * 1000.0;
    let t = cam.target;
    n.orbit_drag(&mut cam, egui::vec2(1.0, 0.0), Some(far), None);
    assert_eq!(n.pivot, Some(t));
    // a near hit — the pivot
    n.pivot = None;
    let p = cam.target + Vec3::new(0.5, 0.0, 0.0);
    n.orbit_drag(&mut cam, egui::vec2(1.0, 0.0), Some(p), None);
    assert_eq!(n.pivot, Some(p));
}

#[test]
fn view_hidden_is_own_set() {
    let mut n = Nav::default();
    let r = n.hidden_rev;
    n.set_view_hidden([Sel::Wall(1)].into());
    assert_eq!(n.hidden_rev, r + 1);
    n.set_view_hidden([Sel::Wall(1)].into());
    assert_eq!(n.hidden_rev, r + 1, "same set: no rebuild");
    assert!(!n.wants_escape());
    n.walk = Some(Walk {
        fly: false,
        mult: 1.0,
    });
    assert!(n.wants_escape());
}

#[test]
fn labels_exist_in_both_languages() {
    for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
        let c = ddc_ui::Catalog::new(lang);
        let l = Labels::load(&c);
        for k in LABEL_KEYS {
            assert_ne!(l.get(k), *k, "{lang:?}: {k}");
        }
    }
}

#[test]
fn walk_speed_is_human_and_flight_is_clamped() {
    let mut n = Nav::default();
    let small = aabb([0.0; 3], [10.0, 10.0, 3.0]);
    let huge = aabb([0.0; 3], [40000.0, 30000.0, 600.0]);
    n.walk = Some(Walk {
        fly: false,
        mult: 1.0,
    });
    // walking — human pace regardless of model size
    assert_eq!(n.walk_speed(Some(&huge)), n.walk_speed(Some(&small)));
    n.walk = Some(Walk {
        fly: true,
        mult: 1.0,
    });
    assert!(n.walk_speed(Some(&huge)) > n.walk_speed(Some(&small)));
    n.walk = Some(Walk {
        fly: true,
        mult: 1000.0,
    });
    assert!(n.walk_speed(Some(&huge)) <= ddc_render::nav::MAX_SPEED);
}

#[test]
fn frame_target_prefers_selection() {
    let bb = |a: f32, b: f32| Aabb {
        min: Vec3::splat(a),
        max: Vec3::splat(b),
    };
    let (sel, all) = (bb(1.0, 2.0), bb(0.0, 100.0));
    assert_eq!(Nav::frame_target(Some(sel), None, Some(all), None), Some(sel));
    assert_eq!(Nav::frame_target(None, None, Some(all), None), Some(all));
    assert_eq!(Nav::frame_target(None, None, None, None), None);
}
