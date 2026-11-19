use super::*;

fn body(h: f32) -> Arc<SceneMesh> {
    Arc::new(SceneMesh {
        element: 0,
        tag: 0,
        positions: vec![[0., 0., 0.], [1., 1., h]],
        triangles: vec![],
    })
}

fn places(n: usize, step: f32) -> Vec<Inst> {
    (0..n)
        .map(|i| Inst {
            m: Mat4::from_translation(Vec3::new(i as f32 * step, (i % 7) as f32, 0.)),
            element: i as u32,
        })
        .collect()
}

fn full_walk(g: &Instanced) -> (Aabb, Aabb) {
    let (mut all, mut tall) = (Aabb::EMPTY, Aabb::EMPTY);
    for i in g.inst.iter() {
        let bx = g.world_bounds(i);
        all.union(&bx);
        if bx.max.z - bx.min.z >= 1.0 {
            tall.union(&bx);
        }
    }
    (all, tall)
}

#[test]
fn cached_bounds_equal_full_walk() {
    let g = Instanced::new(1, body(3.0), places(500, 2.5));
    let (all, tall) = full_walk(&g);
    let a = g.placed_bounds();
    assert_eq!((a.all, a.tall), (all, tall));
    let b = g.placed_bounds();
    assert_eq!(a, b);
    assert_eq!(g.placed_walks(), 1);
    let s = Scene::from_parts(vec![], vec![]).with_instanced(vec![g]);
    assert_eq!(s.bounds, Some(all));
    assert_eq!(s.focus, Some(all));
}

#[test]
fn shared_inst_arc_not_rewalked() {
    let g = Instanced::new(2, body(3.0), places(300, 1.5));
    let mut s = Scene::from_parts(vec![], vec![]).with_instanced(vec![g.clone()]);
    assert_eq!(g.placed_walks(), 0, "the original is not touched");
    let walks = s.instanced[0].placed_walks();
    assert_eq!(walks, 1);
    s.exclude_from_focus(99);
    s.exclude_from_focus(98);
    assert_eq!(s.instanced[0].placed_walks(), walks);
    // a group over the same shared list (another proto handle, same Arc) reuses a cloned cache
    let h = Instanced::shared(2, g.proto.clone(), g.inst.clone());
    let s2 = Scene::from_parts(vec![], vec![]).with_instanced(vec![h]);
    assert_eq!(s2.bounds, s.bounds);
    let c = s.instanced[0].clone();
    let s3 = Scene::from_parts(vec![], vec![]).with_instanced(vec![c]);
    assert_eq!(s3.instanced[0].placed_walks(), walks);
    assert_eq!(s3.bounds, s.bounds);
}

#[test]
fn replaced_inst_recomputes() {
    let g = Instanced::new(3, body(3.0), places(100, 2.0));
    let first = g.placed_bounds();
    // another list with the same length: different Arc, different result
    let moved = Instanced {
        inst: Arc::new(places(100, 5.0)),
        ..g.clone()
    };
    let b = moved.placed_bounds();
    assert_ne!(b.all, first.all);
    assert_eq!((b.all, b.tall), full_walk(&moved));
    assert_eq!(moved.placed_walks(), 2);
    // a filtered (shorter) list
    let short = Instanced {
        inst: Arc::new(places(10, 2.0)),
        ..g.clone()
    };
    assert_eq!(
        (short.placed_bounds().all, short.placed_bounds().tall),
        full_walk(&short)
    );
    // the untouched original still answers from its own cache
    assert_eq!(g.placed_bounds(), first);
    assert_eq!(g.placed_walks(), 1);
    // a changed local box invalidates too
    let mut taller = g.clone();
    taller.local = transform_box(&taller.local, &Mat4::from_scale(Vec3::new(1., 1., 4.)));
    assert_eq!(
        (taller.placed_bounds().all, taller.placed_bounds().tall),
        full_walk(&taller)
    );
    assert_ne!(taller.placed_bounds().all, first.all);
    // empty list
    let none = Instanced::new(4, body(3.0), vec![]);
    assert!(none.placed_bounds().all.is_empty());
    assert!(none.placed_bounds().tall.is_empty());
}

#[test]
fn focus_tall_and_all_match_old() {
    // flat group (0.2 m) plus a tall group (3 m) plus a tall mesh of the excluded element
    let flat = Instanced::new(5, body(0.2), places(50, 3.0));
    let tall = Instanced::new(6, body(3.0), places(20, 4.0));
    let mesh = |element: u32, h: f32, x: f32| SceneMesh {
        element,
        tag: 0,
        positions: vec![[x, 0., 0.], [x + 1., 1., h]],
        triangles: vec![],
    };
    let (m1, m2) = (mesh(7, 50.0, 500.0), mesh(8, 0.5, -300.0));
    let boxes: Vec<Aabb> = [&m1, &m2].iter().map(|m| mesh_box_of(m)).collect();
    let mk = |groups: Vec<Instanced>| {
        let mut s = Scene::from_parts(vec![Arc::new(m1.clone()), Arc::new(m2.clone())], boxes.clone())
            .with_instanced(groups);
        s.exclude_from_focus(7);
        s
    };
    let s = mk(vec![flat.clone(), tall.clone()]);
    // old behaviour: union of per-placement boxes
    let (_, t) = full_walk(&tall);
    assert_eq!(s.focus, Some(t));
    // with only flat groups and the short mesh, `all` is used
    let s = mk(vec![flat.clone()]);
    let (fa, _) = full_walk(&flat);
    let mut want = fa;
    want.union(&boxes[1]);
    assert_eq!(s.focus, Some(want));
    // nothing left: falls back to the full bounds
    let mut only = Scene::from_parts(vec![Arc::new(m1.clone())], vec![boxes[0]]);
    only.exclude_from_focus(7);
    assert_eq!(only.focus, only.bounds);
}

fn mesh_box_of(m: &SceneMesh) -> Aabb {
    let mut b = Aabb::EMPTY;
    m.positions.iter().for_each(|p| b.grow(Vec3::from(*p)));
    b
}

#[test]
#[ignore = "release timing: cargo test --release -p ddc-render with_instanced_1m -- --ignored --nocapture"]
fn with_instanced_1m_placements_budget() {
    let g = Instanced::new(9, body(3.0), places(1_000_000, 1.1));
    let t = std::time::Instant::now();
    let mut s = Scene::from_parts(vec![], vec![]).with_instanced(vec![g]);
    let first = t.elapsed();
    let t = std::time::Instant::now();
    for e in 0..10 {
        s = Scene::from_parts(vec![], vec![]).with_instanced(s.instanced.clone());
        s.exclude_from_focus(e);
    }
    let again = t.elapsed() / 10;
    println!("first build {first:?}, cached rebuild {again:?}");
    assert!(again.as_millis() < 5, "cached rebuild {again:?}");
}
