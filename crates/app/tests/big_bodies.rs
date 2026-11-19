// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! 3D pick bodies after an edit: `Bodies::update` reuses the bodies of unchanged meshes and
//! placements and equals a full build (same bodies, order, hits).
//! Budget at 1M placements, in release:
//! cargo test -p ddc-app --release --test big_bodies -- --ignored --nocapture

use ddc_app::edit3d::pick::{Bodies, Cat, Query, Src, Target, ALL};
use ddc_app::editor::{Editor, OpFn};
use ddc_app::scene_ids::EXT_BASE;
use ddc_app::{mep3d, spatial};
use ddc_model::{Model, WallSpec};
use ddc_render::glam::{Mat4, Vec3};
use ddc_render::{Inst, Instanced, Ray, Scene, SceneMesh};
use std::sync::Arc;
use std::time::Instant;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn cube(e: u32, o: Vec3) -> SceneMesh {
    let positions = (0..8u32)
        .map(|k| {
            [
                o.x + (k & 1) as f32,
                o.y + ((k >> 1) & 1) as f32,
                o.z + ((k >> 2) & 1) as f32,
            ]
        })
        .collect();
    let mut triangles = vec![];
    for [a, b, c, d] in [
        [0, 1, 3, 2],
        [4, 6, 7, 5],
        [0, 4, 5, 1],
        [2, 3, 7, 6],
        [0, 2, 6, 4],
        [1, 5, 7, 3],
    ] {
        triangles.push([a, b, c]);
        triangles.push([a, c, d]);
    }
    SceneMesh {
        element: e,
        tag: 0,
        positions,
        triangles,
    }
}

/// `n` placements of `proto` in a row at `y`, elements from `base`.
fn group(key: u64, proto: &Arc<SceneMesh>, base: u32, n: usize, y: f32) -> Instanced {
    let inst: Vec<Inst> = (0..n)
        .map(|k| Inst {
            m: Mat4::from_translation(Vec3::new(k as f32 * 2.0, y, 0.0)),
            element: base + k as u32,
        })
        .collect();
    Instanced::shared(key, proto.clone(), Arc::new(inst))
}

/// `n` free-standing walls along x (no joins: editing one leaves the others' geometry as is).
fn model(n: usize) -> (Editor, Vec<i64>) {
    let mut ed = Editor::new_in_memory().unwrap();
    let (level, wt) = (ed.level, ed.types[0].id);
    let ws: Vec<WallSpec> = (0..n)
        .map(|i| WallSpec {
            a: (i as f64 * 6.0, 0.0),
            b: (i as f64 * 6.0 + 3.0, 0.0),
            level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: wt,
            offset0: None,
        })
        .collect();
    let ids = ed.model.add_walls(&ws).unwrap();
    ed.reload().unwrap();
    (ed, ids)
}

fn op(
    ed: &mut Editor,
    f: impl Fn(&mut Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) {
    ed.run_op(OpFn(Arc::new(f))).unwrap();
}

fn wall_rays(n: usize) -> Vec<Ray> {
    let mut v = vec![Ray {
        origin: Vec3::new(-10.0, 0.0, 1.5),
        dir: Vec3::X,
    }];
    for i in 0..n * 24 {
        let x = i as f32 * 0.25 + 0.1;
        v.push(Ray {
            origin: Vec3::new(x, -5.0, 1.5),
            dir: Vec3::Y,
        });
        v.push(Ray {
            origin: Vec3::new(x, 0.01, 10.0),
            dir: Vec3::NEG_Z,
        });
    }
    v
}

fn grid_rays() -> Vec<Ray> {
    let mut v = vec![];
    for i in 0..130 {
        for y in [-13.5f32, -11.5, -9.5, 10.5, 14.5, 20.5, 30.5] {
            v.push(Ray {
                origin: Vec3::new(i as f32 * 0.5 - 1.25, y, 5.0),
                dir: Vec3::NEG_Z,
            });
        }
    }
    v
}

fn src_eq(a: &Src, b: &Src) -> bool {
    match (a, b) {
        (Src::Scene(x), Src::Scene(y)) => x == y,
        (Src::Inst { g, k }, Src::Inst { g: h, k: j }) => g == h && k == j,
        (Src::Own { pos, tris }, Src::Own { pos: p, tris: t }) => pos == p && tris == t,
        _ => false,
    }
}

/// Same bodies in the same order, same lookups, same hits with and without the scene index.
fn assert_same(up: &Bodies, full: &Bodies, sc: &Scene, rays: &[Ray]) {
    assert_eq!(up.list.len(), full.list.len());
    for (i, (a, b)) in up.list.iter().zip(&full.list).enumerate() {
        assert_eq!(a.target, b.target, "body {i}");
        assert_eq!(a.aabb, b.aabb, "body {i}");
        assert_eq!(a.rank, b.rank, "body {i}");
        assert!(src_eq(&a.src, &b.src), "body {i}");
        if let Some(t) = b.target {
            assert_eq!(up.of(t), full.of(t), "{t:?}");
        }
    }
    let ix = spatial::Index::build(sc);
    let q = Query {
        index: Some(&ix),
        clip: None,
        full: true,
    };
    let mut any = 0;
    for r in rays {
        let h = full.hits(sc, r);
        any += usize::from(!h.is_empty());
        assert_eq!(up.hits(sc, r), h, "{r:?}");
        assert_eq!(up.hits_in(sc, r, &q), full.hits_in(sc, r, &q), "{r:?}");
    }
    assert!(any > 0, "rays hit something");
}

#[test]
fn update_equals_full_build_after_edit() {
    // model walls through the cached scene: one wall moved
    let (mut ed, ids) = model(6);
    let s0 = mep3d::scene(&mut ed);
    let b0 = Bodies::build_with(&ed, &s0, &[]);
    let moved = ids[2];
    op(&mut ed, move |m| m.set_wall_axis(moved, (12.0, 4.0), (15.0, 4.0)));
    let s1 = mep3d::scene(&mut ed);
    let up = Bodies::update(Some(b0), &ed, &s1, &[]);
    let full = Bodies::build_with(&ed, &s1, &[]);
    assert_same(&up, &full, &s1, &wall_rays(6));
    assert!(up.reused > 0);

    // meshes and placement groups: a mesh moved, one removed, one added; one group gets a new
    // placement list, one stays (now first), one is added
    let e = |i: u32| EXT_BASE + i;
    let proto = Arc::new(cube(0, Vec3::ZERO));
    let meshes: Vec<Arc<SceneMesh>> = (0..10)
        .map(|i| Arc::new(cube(e(i), Vec3::new(i as f32 * 2.0, -10.0, 0.0))))
        .chain([Arc::new(cube(e(3), Vec3::new(6.0, -12.0, 0.0)))])
        .collect();
    let g0 = group(1, &proto, e(100), 20, 10.0);
    let g1 = group(2, &proto, e(200), 5, 20.0);
    let a = Scene::shared(meshes.clone()).with_instanced(vec![g0.clone(), g1.clone()]);
    let ts: Vec<Target> = (0..300).map(|i| Target::new(Cat::Column, i)).collect();
    let ed = Editor::new_in_memory().unwrap();
    let b0 = Bodies::build_with(&ed, &a, &ts);
    assert_eq!((b0.reused, b0.rebuilt), (0, 10 + 20 + 5));
    let mut m2 = meshes.clone();
    m2[4] = Arc::new(cube(e(4), Vec3::new(8.0, -14.0, 0.0)));
    m2.remove(7);
    m2.push(Arc::new(cube(e(50), Vec3::new(40.0, -10.0, 0.0))));
    let mut inst0 = g0.inst.to_vec();
    inst0[3].m = Mat4::from_translation(Vec3::new(6.0, 14.0, 0.0));
    let g0b = Instanced::shared(1, proto.clone(), Arc::new(inst0));
    let g2 = group(3, &proto, e(250), 4, 30.0);
    let b = Scene::shared(m2).with_instanced(vec![g1.clone(), g0b, g2]);
    let up = Bodies::update(Some(b0), &ed, &b, &ts);
    let full = Bodies::build_with(&ed, &b, &ts);
    assert_same(&up, &full, &b, &grid_rays());
    // elements 0..9 without 4 and 7 (element 3 has two meshes), and group 1
    assert_eq!(up.reused, 8 + 5);
    assert_eq!(up.rebuilt, 1 + 1 + 20 + 4);
    assert!(up.of(Target::new(Cat::Column, 7)).is_none());

    // nothing changed: everything reused, still equal
    let again = Bodies::update(Some(up), &ed, &b, &ts);
    assert_same(&again, &full, &b, &grid_rays());
    assert_eq!((again.reused, again.rebuilt), (8 + 5 + 2 + 20 + 4, 0));
}

#[test]
fn update_after_one_wall_edit_touches_one_body() {
    let (mut ed, ids) = model(6);
    let s0 = mep3d::scene(&mut ed);
    let b0 = Bodies::build_with(&ed, &s0, &[]);
    let moved = ids[2];
    let before = b0.list[b0.of(Target::new(Cat::Wall, moved)).unwrap()].aabb;
    assert!(before.max.y < 3.0);
    op(&mut ed, move |m| m.set_wall_axis(moved, (12.0, 4.0), (15.0, 4.0)));
    let s1 = mep3d::scene(&mut ed);
    let up = Bodies::update(Some(b0), &ed, &s1, &[]);
    let full = Bodies::build_with(&ed, &s1, &[]);
    assert_same(&up, &full, &s1, &wall_rays(6));
    assert_eq!(up.rebuilt, 1, "only the moved wall");
    assert_eq!(up.reused + 1, full.rebuilt);
    let after = up.list[up.of(Target::new(Cat::Wall, moved)).unwrap()].aabb;
    assert!(after.min.y > 3.0, "{after:?}");
}

#[test]
fn update_after_delete_drops_body() {
    let (mut ed, ids) = model(5);
    let s0 = mep3d::scene(&mut ed);
    let b0 = Bodies::build_with(&ed, &s0, &[]);
    let n0 = b0.rebuilt;
    let gone = ids[1];
    assert!(b0.of(Target::new(Cat::Wall, gone)).is_some());
    op(&mut ed, move |m| m.delete_wall(gone));
    let s1 = mep3d::scene(&mut ed);
    let up = Bodies::update(Some(b0), &ed, &s1, &[]);
    let full = Bodies::build_with(&ed, &s1, &[]);
    assert_same(&up, &full, &s1, &wall_rays(5));
    assert!(up.of(Target::new(Cat::Wall, gone)).is_none());
    assert_eq!((up.reused, up.rebuilt), (n0 - 1, 0));
    for &id in ids.iter().filter(|&&id| id != gone) {
        assert!(up.of(Target::new(Cat::Wall, id)).is_some(), "wall {id}");
    }
    // a ray through the deleted wall finds nothing there any more
    let r = Ray {
        origin: Vec3::new(7.5, -5.0, 1.5),
        dir: Vec3::Y,
    };
    assert!(up.hits(&s1, &r).is_empty());
}

#[test]
#[ignore = "1M placements, run in release with --nocapture"]
fn bodies_update_1m_placements_budget() {
    let (groups, per) = (1000usize, 1000usize);
    let proto = Arc::new(cube(0, Vec3::ZERO));
    let gs: Vec<Instanced> = (0..groups)
        .map(|g| {
            let base = EXT_BASE + (g * per) as u32;
            group(g as u64, &proto, base, per, g as f32 * 2.0)
        })
        .collect();
    let ts: Vec<Target> = (0..(groups * per) as i64)
        .map(|i| Target::new(Cat::Mesh, i))
        .collect();
    let ed = Editor::new_in_memory().unwrap();
    let s0 = Scene::new(vec![]).with_instanced(gs.clone());
    let t = Instant::now();
    let b0 = Bodies::build_with(&ed, &s0, &ts);
    let t_full = ms(t);
    // one edit: a placement of group 7 moves (the group gets a new placement list)
    let mut gs2 = gs.clone();
    let mut v = gs[7].inst.to_vec();
    v[0].m = Mat4::from_translation(Vec3::new(-5.0, 14.0, 0.0));
    gs2[7] = Instanced::shared(7, proto.clone(), Arc::new(v));
    let s1 = Scene::new(vec![]).with_instanced(gs2);
    let t = Instant::now();
    let up = Bodies::update(Some(b0), &ed, &s1, &ts);
    let t_up = ms(t);
    let t = Instant::now();
    let full = Bodies::build_with(&ed, &s1, &ts);
    let t_full2 = ms(t);
    println!(
        "bodies 1M placements: full build {t_full:.0} ms, update after one edit {t_up:.0} ms \
         ({} reused, {} rebuilt), full build again {t_full2:.0} ms",
        up.reused, up.rebuilt
    );
    assert_eq!(up.rebuilt, per);
    assert_eq!(up.reused, (groups - 1) * per);
    assert_eq!(up.list.len(), full.list.len());
    for (a, b) in up.list.iter().zip(&full.list) {
        assert!(a.target == b.target && a.aabb == b.aabb && src_eq(&a.src, &b.src));
    }
    let r = Ray {
        origin: Vec3::new(-4.5, 14.5, 5.0),
        dir: Vec3::NEG_Z,
    };
    // no index: a one-off full scan, as a click does while the index builds
    let q = Query { full: true, ..ALL };
    let h = up.hits_in(&s1, &r, &q);
    assert_eq!(h, full.hits_in(&s1, &r, &q));
    assert_eq!(
        h.first().map(|h| h.target),
        Some(Target::new(Cat::Mesh, 7000))
    );
    assert!(t_up < 250.0, "update {t_up:.0} ms over the 250 ms budget");
}
