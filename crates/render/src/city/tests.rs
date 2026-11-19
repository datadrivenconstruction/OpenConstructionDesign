use super::cook::{cook, Kid};
use super::gen::{city_projects, totals, Building, CityProject, CitySpec, Kind, Protos};
use super::hlod::{opaque_tag, tri_box, unit_chain, Grid, Label, Source};
use super::project::{content_bytes, offset, place, Project};
use super::*;
use crate::camera::OrbitCamera;
use crate::chunks::box_mesh;
use crate::scene::{mesh_box, tag_rgb, Aabb, SceneMesh};
use glam::{Mat4, Vec3};
use std::sync::Arc;

const PX: (u32, u32) = (1920, 1080);

fn small() -> CitySpec {
    CitySpec::with_blocks(2)
}

fn cube(min: Vec3, max: Vec3, element: u32) -> (Arc<SceneMesh>, Aabb) {
    let m = box_mesh(&Aabb { min, max }, element, tag_rgb([200, 0, 0], false));
    let b = mesh_box(&m);
    (Arc::new(m), b)
}

/// The labels cooking uses: one owner, opaque colours.
fn label(e: u32) -> impl Fn(u32, u32) -> Label {
    move |_, tag| Label {
        element: e,
        tag: opaque_tag(tag),
    }
}

fn view_of(projects: Vec<Arc<CityProject>>, cfg: CityConfig) -> CityView {
    CityView::new(
        projects
            .into_iter()
            .map(|p| p as Arc<dyn Project>)
            .collect(),
        cfg,
    )
}

/// Camera looking at `b` from its fitting distance times `k`.
fn cam_at(b: &Aabb, k: f32) -> (Mat4, Vec3) {
    let mut c = OrbitCamera::default();
    c.fit(b, PX.0 as f32 / PX.1 as f32);
    c.distance *= k;
    (c.view_proj(PX.0 as f32 / PX.1 as f32), c.eye())
}

fn settle(v: &mut CityView, vp: Mat4, eye: Vec3) {
    for _ in 0..400 {
        v.update(vp, eye, PX, None);
        if v.settled() {
            return;
        }
    }
    panic!("city did not settle: {:?}", v.stats());
}

fn tower_project(floors: u32) -> CityProject {
    let b = Building::new(
        Kind::Tower,
        Vec3::new(10.0, 10.0, 0.0),
        Vec3::new(24.0, 21.0, 0.0),
        floors,
        7,
    );
    CityProject::from_buildings([1000.0, 2000.0, 0.0], vec![b], Arc::new(Protos::new()))
}

#[test]
fn tri_box_separates_and_touches() {
    let c = Vec3::ZERO;
    let h = Vec3::splat(0.5);
    let t = |a: [f32; 3], b: [f32; 3], d: [f32; 3]| [a.into(), b.into(), d.into()];
    // through the centre
    assert!(tri_box(c, h, t([-2.0, -2.0, 0.0], [2.0, -2.0, 0.0], [0.0, 2.0, 0.0])));
    // a plane above the box
    assert!(!tri_box(c, h, t([-2.0, -2.0, 0.6], [2.0, -2.0, 0.6], [0.0, 2.0, 0.6])));
    // the bounding boxes overlap at the corner, the triangle does not: an edge axis separates
    assert!(!tri_box(c, h, t([1.3, 0.4, 0.0], [0.4, 1.3, 0.0], [1.3, 1.3, 0.0])));
    // touching a face counts
    assert!(tri_box(c, h, t([0.5, -1.0, -1.0], [0.5, 1.0, -1.0], [0.5, 0.0, 1.0])));
}

#[test]
fn box_proxy_is_one_quad_per_face_wound_outward() {
    let (m, b) = cube(Vec3::ZERO, Vec3::new(10.0, 6.0, 4.0), 7);
    let mut g = Grid::new(&b, 1.0);
    g.mark_mesh(&m, &Mat4::IDENTITY, Label { element: 7, tag: 1 });
    let p = g.proxy(0.0);
    assert_eq!(p.triangles, 12, "greedy quads: two triangles per face");
    assert_eq!(p.meshes.len(), 1);
    assert_eq!(p.meshes[0].0.element, 7);
    let centre = Vec3::new(5.0, 3.0, 2.0);
    for (m, _) in &p.meshes {
        for t in &m.triangles {
            let [a, b, c] = t.map(|i| Vec3::from(m.positions[i as usize]));
            let n = (b - a).cross(c - a);
            assert!(n.dot((a + b + c) / 3.0 - centre) > 0.0, "inward face");
        }
    }
    let grow = p.error;
    assert!(p.bounds.min.cmpge(Vec3::splat(-grow)).all());
    assert!(p.bounds.max.cmple(Vec3::new(10.0, 6.0, 4.0) + grow).all());
}

#[test]
fn enclosed_interior_is_filled_and_open_courtyard_stays_open() {
    let t = 0.3;
    let (w, d, h) = (12.0, 12.0, 6.0);
    let walls = vec![
        cube(Vec3::ZERO, Vec3::new(w, t, h), 1),
        cube(Vec3::new(0.0, d - t, 0.0), Vec3::new(w, d, h), 1),
        cube(Vec3::ZERO, Vec3::new(t, d, h), 1),
        cube(Vec3::new(w - t, 0.0, 0.0), Vec3::new(w, d, h), 1),
    ];
    let mut closed = walls.clone();
    closed.push(cube(Vec3::ZERO, Vec3::new(w, d, t), 1));
    closed.push(cube(Vec3::new(0.0, 0.0, h - t), Vec3::new(w, d, h), 1));
    let proxy = |list: &[(Arc<SceneMesh>, Aabb)]| {
        let src = Source { meshes: list, groups: &[] };
        let mut g = Grid::new(&src.bounds(), 1.0);
        for (m, _) in list {
            g.mark_mesh(m, &Mat4::IDENTITY, Label { element: 1, tag: 1 });
        }
        g.proxy(0.0)
    };
    assert_eq!(proxy(&closed).triangles, 12, "a closed shell becomes one block");
    let open = proxy(&walls);
    assert!(open.triangles > 12, "the courtyard open to the sky keeps its inner faces");
}

#[test]
fn proxy_surface_stays_within_its_error() {
    let mut list = vec![];
    let mut s = 0x1234_5678u64;
    let mut r = || {
        s = project::mix(s);
        (s >> 40) as f32 / (1u64 << 24) as f32
    };
    for e in 0..40 {
        let min = Vec3::new(r() * 50.0, r() * 50.0, r() * 10.0);
        let size = Vec3::new(0.1 + r() * 6.0, 0.1 + r() * 6.0, 0.1 + r() * 20.0);
        list.push(cube(min, min + size, e));
    }
    let src = Source { meshes: &list, groups: &[] };
    let chain = unit_chain(&src, &label(1));
    assert!(!chain.is_empty());
    for p in &chain {
        for (m, _) in &p.meshes {
            for v in &m.positions {
                let v = Vec3::from(*v);
                let d = list
                    .iter()
                    .map(|(_, b)| (v.clamp(b.min, b.max) - v).length())
                    .fold(f32::INFINITY, f32::min);
                assert!(d <= p.error + 1e-3, "vertex {v} is {d} m off, error {}", p.error);
            }
        }
    }
    for w in chain.windows(2) {
        assert!(w[1].error > w[0].error, "errors grow up the chain");
        assert!(w[1].triangles as f32 <= w[0].triangles as f32 * hlod::KEEP_RATIO);
    }
}

#[test]
fn generator_is_deterministic_and_plans_match_loads() {
    let a = city_projects(&small());
    let b = city_projects(&small());
    assert_eq!(totals(&a), totals(&b));
    let mut other = small();
    other.seed ^= 1;
    let c = city_projects(&other);
    let hash = |ps: &[Arc<CityProject>]| {
        let mut h = 0u64;
        for p in ps {
            for (u, _) in p.units().iter().enumerate() {
                let c = p.load(u);
                for m in &c.meshes {
                    h = project::mix(h ^ crate::chunks::mesh_fp(m));
                }
                for g in &c.groups {
                    for i in g.inst.iter() {
                        h = project::mix(h ^ u64::from(i.element) ^ i.m.w_axis.x.to_bits() as u64);
                    }
                }
            }
        }
        h
    };
    assert_eq!(hash(&a), hash(&b), "same seed, same geometry");
    assert_ne!(hash(&a), hash(&c), "another seed, another city");
    for p in &a {
        for (u, unit) in p.units().iter().enumerate() {
            let c = p.load(u);
            let placed = place(c.clone(), Vec3::ZERO, 0, 0);
            assert_eq!(placed.bytes, unit.bytes, "planned bytes = loaded bytes");
            let mut ids: Vec<u32> = c.meshes.iter().map(|m| m.element).collect();
            for g in &c.groups {
                ids.extend(g.inst.iter().map(|i| i.element));
            }
            ids.sort_unstable();
            let want: Vec<u32> = unit.elements.clone().collect();
            assert_eq!(ids, want, "unit elements are exactly its range");
            let src = Source { meshes: &placed.meshes, groups: &placed.groups };
            assert_eq!(src.bounds(), unit.bounds);
            assert_eq!(content_bytes(&placed.meshes, &placed.groups), placed.bytes);
        }
    }
}

#[test]
fn presets_reach_thousands_of_buildings() {
    let s = city_projects(&CitySpec::preset("s").expect("preset"));
    let (buildings, units, elements, _) = totals(&s);
    assert_eq!(s.len(), 64, "16 x 16 blocks, 2 x 2 per project");
    assert!(buildings >= 1000, "{buildings} buildings");
    assert!(units >= 400, "{units} units");
    assert!(elements >= 300_000, "{elements} elements");
    assert!(CitySpec::preset("x").is_none());
}

#[test]
fn prototypes_are_shared_across_projects() {
    let ps = city_projects(&CitySpec::with_blocks(4));
    let mut seen: std::collections::HashMap<u64, usize> = Default::default();
    let mut groups = 0;
    for p in &ps {
        for (u, _) in p.units().iter().enumerate() {
            for g in p.load(u).groups {
                groups += 1;
                let ptr = Arc::as_ptr(&g.proto) as usize;
                let e = seen.entry(g.key).or_insert(ptr);
                assert_eq!(*e, ptr, "one mesh per prototype in the whole city");
            }
        }
    }
    assert!(groups > seen.len(), "instanced groups reuse prototypes");
}

#[test]
fn tower_proxy_is_far_cheaper_than_the_tower() {
    let p = tower_project(20);
    let placed = place(p.load(0), Vec3::ZERO, 0, 0);
    let src = Source { meshes: &placed.meshes, groups: &placed.groups };
    let chain = unit_chain(&src, &label(1));
    assert!(placed.triangles > 30_000, "{} source triangles", placed.triangles);
    assert!(
        chain[0].triangles * 10 < placed.triangles,
        "finest proxy {} vs {} triangles",
        chain[0].triangles,
        placed.triangles
    );
    // glass is opaque in proxies
    for (m, _) in &chain[0].meshes {
        assert!(!crate::scene::is_translucent(m.tag));
    }
}

#[test]
fn errors_grow_up_the_tree() {
    let ps = city_projects(&CitySpec::with_blocks(4));
    for p in ps.iter().take(2) {
        let c = cook(p.as_ref(), Vec3::ZERO, 0, 1);
        for n in &c.nodes {
            for k in &n.kids {
                let e = match *k {
                    Kid::Node(m) => c.nodes[m as usize].proxy.error,
                    Kid::Unit(u) => c.units[u as usize].coarsest().map_or(0.0, |x| x.error),
                };
                assert!(n.proxy.error >= e, "node {} below kid {e}", n.proxy.error);
            }
        }
        for u in &c.units {
            assert!(u.levels.windows(2).all(|w| w[1].error >= w[0].error));
            assert!(!u.levels.is_empty());
        }
        assert!(c.proxy_triangles < c.source_triangles);
    }
}

/// Units each piece of the cut stands for.
fn covered(v: &CityView) -> Vec<(u32, u32)> {
    let mut out = vec![];
    for pc in v.cut() {
        match *pc {
            Piece::Box(p) => {
                let n = v.members[p as usize].stand_in.0.triangles.len();
                assert_eq!(n, 12);
                out.push((p, u32::MAX));
            }
            Piece::Node(p, n) => {
                let c = v.cooked(p);
                let mut stack = vec![Kid::Node(n)];
                while let Some(k) = stack.pop() {
                    match k {
                        Kid::Node(m) => stack.extend(c.nodes[m as usize].kids.iter().copied()),
                        Kid::Unit(u) => out.push((p, u)),
                    }
                }
            }
            Piece::Level(p, u, _) | Piece::Full(p, u) => out.push((p, u)),
        }
    }
    out
}

#[test]
fn cut_covers_every_unit_once_from_any_camera() {
    let mut v = view_of(city_projects(&CitySpec::with_blocks(4)), CityConfig {
        jobs_per_update: 64,
        ..CityConfig::default()
    });
    let b = v.bounds().expect("bounds");
    let first = v.update(cam_at(&b, 1.0).0, cam_at(&b, 1.0).1, PX, None);
    assert!(first.is_some(), "first update gives a scene");
    for k in [1.0, 0.3, 0.1, 0.02, 0.005] {
        let (vp, eye) = cam_at(&b, k);
        settle(&mut v, vp, eye);
        let mut all = covered(&v);
        let n = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), n, "a unit drawn twice at k = {k}");
        assert_eq!(n, v.stats().units, "a unit missing at k = {k}");
        assert!(v.update(vp, eye, PX, None).is_none(), "same camera, same cut");
    }
    assert!(v.cut().iter().any(|p| matches!(p, Piece::Full(..))), "close view loads units");
}

#[test]
fn far_view_draws_few_triangles_and_close_view_streams() {
    let mut v = view_of(city_projects(&CitySpec::with_blocks(4)), CityConfig {
        jobs_per_update: 64,
        ..CityConfig::default()
    });
    let b = v.bounds().expect("bounds");
    let (vp, eye) = cam_at(&b, 3.0);
    settle(&mut v, vp, eye);
    let far = v.stats();
    assert_eq!(far.full_units, 0, "nothing loads for a far view");
    let total: usize = v
        .members
        .iter()
        .filter_map(|m| m.cooked.as_ref())
        .map(|c| c.source_triangles)
        .sum();
    assert!(far.triangles * 5 < total, "far view {} of {total} triangles", far.triangles);
}

#[test]
fn memory_budget_holds_on_a_dive() {
    let ps = city_projects(&CitySpec::with_blocks(4));
    let all: u64 = totals(&ps).3;
    let budget = all / 6;
    let mut v = view_of(ps, CityConfig {
        budget,
        jobs_per_update: 64,
        ..CityConfig::default()
    });
    let b = v.bounds().expect("bounds");
    let mut held = 0;
    let mut full = 0;
    for i in 0..60 {
        let k = 1.0 * (0.002f32).powf(i as f32 / 59.0);
        let (vp, eye) = cam_at(&b, k);
        for _ in 0..3 {
            v.update(vp, eye, PX, None);
            let s = v.stats();
            assert!(s.full_bytes <= budget, "{} over budget {budget}", s.full_bytes);
            held = held.max(s.held);
            full = full.max(s.full_units);
        }
    }
    assert!(full > 0, "units streamed in");
    let (vp, eye) = cam_at(&b, 0.15);
    settle(&mut v, vp, eye);
    let s = v.stats();
    assert!(s.full_bytes <= budget);
    assert!(s.held > 0 || held > 0, "a small budget holds units back: {s:?}");
    // a larger budget lets them in
    v.set_budget(all * 2);
    settle(&mut v, vp, eye);
    assert_eq!(v.stats().held, 0);
}

#[test]
fn camera_facing_away_refines_nothing() {
    let mut v = view_of(city_projects(&CitySpec::with_blocks(4)), CityConfig {
        jobs_per_update: 64,
        ..CityConfig::default()
    });
    let b = v.bounds().expect("bounds");
    let c = (b.min + b.max) * 0.5;
    let eye = Vec3::new(c.x, b.min.y - 5.0, 2.0);
    let look = Mat4::look_at_rh(eye, eye - Vec3::Y, Vec3::Z);
    let vp = Mat4::perspective_infinite_reverse_rh(0.8, 16.0 / 9.0, 0.01) * look;
    settle(&mut v, vp, eye);
    let s = v.stats();
    assert_eq!(s.full_units, 0);
    assert!(s.culled > 0);
    assert!(v.cut().iter().all(|p| matches!(p, Piece::Node(..))), "only closed roots");
}

#[test]
fn occluded_projects_are_not_refined() {
    let mut v = view_of(city_projects(&small()), CityConfig::default());
    let b = v.bounds().expect("bounds");
    let (vp, eye) = cam_at(&b, 1.0);
    // cook first, without occlusion
    settle(&mut v, vp, eye);
    let open = v.cut().len();
    assert!(open > 1, "the fitted view refines the root");
    let mut w = view_of(city_projects(&small()), CityConfig::default());
    let wall = crate::perf::HiZ::new(16, 9, vec![1.0; 16 * 9], vp, eye);
    for _ in 0..50 {
        w.update(vp, eye, PX, Some(&wall));
    }
    assert_eq!(w.cut().len(), 1, "behind a wall only the root proxy stays: {:?}", w.cut());
    assert!(w.stats().occluded > 0);
}

#[test]
fn ids_are_global_and_map_back_to_projects() {
    let ps = city_projects(&CitySpec::with_blocks(4));
    let ends: Vec<u32> = ps.iter().map(|p| p.elements()).collect();
    let v = view_of(ps.clone(), CityConfig::default());
    for (p, proj) in ps.iter().enumerate() {
        for u in proj.units() {
            for local in [u.elements.start, u.elements.end - 1] {
                let g = local + v.members[p].base;
                assert_eq!(v.project_of(g), Some((p, local)));
            }
        }
        assert!(v.members[p].end >= v.members[p].base + ends[p] - 1);
        if p > 0 {
            assert!(v.members[p].base > v.members[p - 1].end, "ranges never overlap");
        }
    }
}

#[test]
fn far_origins_place_exactly() {
    let anchor = [512_123.3, 6_200_456.7, 35.0];
    let d = [1234.567, -987.123, 0.0];
    let o = [anchor[0] + d[0], anchor[1] + d[1], anchor[2] + d[2]];
    let off = offset(o, anchor);
    let want = Vec3::new(d[0] as f32, d[1] as f32, 0.0);
    assert!((off - want).length() < 1e-4, "{off} vs {want}");
}

#[test]
fn hidden_project_leaves_the_cut_in_one_update() {
    let mut v = view_of(city_projects(&CitySpec::with_blocks(4)), CityConfig::default());
    let b = v.bounds().expect("bounds");
    let (vp, eye) = cam_at(&b, 1.0);
    settle(&mut v, vp, eye);
    v.set_visible(0, false);
    assert!(v.update(vp, eye, PX, None).is_some());
    assert!(covered(&v).iter().all(|&(p, _)| p != 0));
    v.set_visible(0, true);
    assert!(v.update(vp, eye, PX, None).is_some());
    assert!(covered(&v).iter().any(|&(p, _)| p == 0));
}

#[test]
fn worker_threads_cook_and_load() {
    let mut v = view_of(city_projects(&CitySpec::with_blocks(4)), CityConfig {
        workers: 2,
        ..CityConfig::default()
    });
    let b = v.bounds().expect("bounds");
    let (vp, eye) = cam_at(&b, 0.05);
    let t = std::time::Instant::now();
    while !v.settled() || v.stats().cooked < v.project_count() {
        v.update(vp, eye, PX, None);
        assert!(t.elapsed().as_secs() < 300, "workers stalled: {:?}", v.stats());
        std::thread::yield_now();
    }
    assert!(v.stats().full_units > 0);
    let scene = v.compose();
    assert!(scene.triangle_count() > 0 || !scene.instanced.is_empty());
}

#[test]
fn units_without_geometry_never_break_the_tree() {
    let empty = CityProject::from_buildings([0.0; 3], vec![], Arc::new(Protos::new()));
    let mut v = view_of(vec![Arc::new(empty), Arc::new(tower_project(3))], CityConfig::default());
    let b = v.bounds().expect("bounds");
    let (vp, eye) = cam_at(&b, 0.5);
    settle(&mut v, vp, eye);
    assert_eq!(v.stats().cooked, 2);
}
