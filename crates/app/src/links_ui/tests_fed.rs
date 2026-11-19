//! Federation: link cache (miss, hit, stale, damaged, eviction, data safety), parallel loads,
//! own render sources (host edits leave link groups alone, moves only rewrite matrices).

use super::*;
use crate::modify_tools::tests::walls;
use ddc_model::links::Body;
use std::path::Path;
use std::sync::atomic::AtomicBool;

type Segs = [((f64, f64), (f64, f64))];

fn tmp(tag: &str) -> PathBuf {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let d = std::env::temp_dir().join(format!("ddc-fed-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ifc_source(p: &Path, segs: &Segs) {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, segs);
    std::fs::write(p, ddc_io::ifc::export_ifc(&ed.model).unwrap()).unwrap();
}

fn load_with(p: &Path, dir: Option<&Path>) -> load::LinkData {
    let c = dir.map(|d| load::CacheUse {
        dir: d.to_path_buf(),
        rehash: false,
    });
    let cancel = Arc::new(AtomicBool::new(false));
    load::load(p, "ifc", None, c.as_ref(), &cancel, &mut |_, _| {}).unwrap()
}

fn assert_same(a: &load::LinkData, b: &load::LinkData) {
    assert_eq!(a.levels, b.levels);
    assert_eq!(a.walls, b.walls);
    assert_eq!(a.elems.len(), b.elems.len());
    for (x, y) in a.elems.iter().zip(b.elems.iter()) {
        assert_eq!((x.kind, x.id, &x.guid), (y.kind, y.id, &y.guid));
        assert_eq!(x.body, y.body);
    }
    assert_eq!(a.source.shapes.len(), b.source.shapes.len());
    for (x, y) in a.source.shapes.iter().zip(&b.source.shapes) {
        assert_eq!(x.places, y.places);
        assert_eq!(x.mesh.positions, y.mesh.positions);
        assert_eq!(x.mesh.triangles, y.mesh.triangles);
    }
}

fn stamp(p: &Path) -> (Vec<u8>, SystemTime) {
    let m = std::fs::metadata(p).unwrap();
    (std::fs::read(p).unwrap(), m.modified().unwrap())
}

fn ddc_files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".ddc"))
        .collect();
    v.sort();
    v
}

#[test]
fn cache_miss_then_hit_then_stale_and_source_untouched() {
    let dir = tmp("cache");
    let src = dir.join("site.ifc");
    ifc_source(&src, &[((0.0, 0.0), (5.0, 0.0)), ((5.0, 0.0), (5.0, 4.0))]);
    let before = stamp(&src);
    let cdir = cache::dir_for(&dir.join("house.ddc"));
    assert_eq!(cdir, dir.join("house.links"));
    let cold = load_with(&src, Some(&cdir));
    assert!(!cold.hit, "first open imports");
    let name = cold.cache.clone().expect("cache written");
    assert!(cdir.join(&name).is_file());
    let warm = load_with(&src, Some(&cdir));
    assert!(warm.hit, "reopen reads the cache");
    assert_eq!(warm.cache.as_deref(), Some(name.as_str()));
    assert_same(&cold, &warm);
    // the cache file is a native project: it opens as a model with the imported walls
    let m = Model::open(cdir.join(&name).to_str().unwrap()).unwrap();
    let n: i64 = m
        .sql()
        .query_row("SELECT COUNT(*) FROM wall", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
    drop(m);
    // data safety: the source is never written, nothing new lands beside it but the folder
    assert_eq!(stamp(&src), before);
    let mut beside: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    beside.sort();
    assert_eq!(
        beside,
        vec!["house.links".to_string(), "site.ifc".to_string()]
    );
    // stale: other content → other key → import again
    ifc_source(
        &src,
        &[
            ((0.0, 0.0), (5.0, 0.0)),
            ((0.0, 2.0), (5.0, 2.0)),
            ((0.0, 4.0), (5.0, 4.0)),
        ],
    );
    let fresh = load_with(&src, Some(&cdir));
    assert!(!fresh.hit, "changed source is a miss");
    assert_ne!(fresh.cache.as_deref(), Some(name.as_str()));
    assert_eq!(fresh.walls.len(), 3);
    assert!(load_with(&src, Some(&cdir)).hit);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn damaged_cache_is_a_miss_and_is_rebuilt() {
    let dir = tmp("damaged");
    let src = dir.join("a.ifc");
    ifc_source(&src, &[((0.0, 0.0), (5.0, 0.0))]);
    let cdir = dir.join("p.links");
    let name = load_with(&src, Some(&cdir)).cache.unwrap();
    std::fs::write(cdir.join(&name), b"not a database").unwrap();
    let again = load_with(&src, Some(&cdir));
    assert!(!again.hit);
    assert_eq!(again.walls.len(), 1);
    assert!(load_with(&src, Some(&cdir)).hit, "rewritten whole");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn remembered_hash_is_trusted_until_rehash() {
    let dir = tmp("stamp");
    let src = dir.join("a.ifc");
    std::fs::write(&src, b"ISO-10303-21; content").unwrap();
    let cdir = dir.join("p.links");
    let real = cache::hash_file(&src).unwrap();
    assert_eq!(cache::content_hash(&cdir, &src, false).unwrap(), real);
    // a forged memo with the same size and time is trusted; an explicit reload reads anew
    let st = std::fs::read_dir(&cdir)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().ends_with(".stamp"))
        .unwrap()
        .path();
    let text = std::fs::read_to_string(&st).unwrap();
    let forged = text.replace(&real, &"a".repeat(64));
    std::fs::write(&st, forged).unwrap();
    assert_eq!(
        cache::content_hash(&cdir, &src, false).unwrap(),
        "a".repeat(64)
    );
    assert_eq!(cache::content_hash(&cdir, &src, true).unwrap(), real);
    assert_eq!(cache::content_hash(&cdir, &src, false).unwrap(), real);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eviction_deletes_oldest_cache_files_only() {
    let dir = tmp("evict");
    let names: Vec<String> = (0..4)
        .map(|i| format!("{}.{:016x}.ddc", format!("{i:x}").repeat(32), i))
        .collect();
    let t0 = SystemTime::now() - Duration::from_secs(1000);
    for (i, n) in names.iter().enumerate() {
        let p = dir.join(n);
        std::fs::write(&p, vec![0u8; 1000]).unwrap();
        let f = std::fs::File::options().append(true).open(&p).unwrap();
        f.set_modified(t0 + Duration::from_secs(i as u64 * 10))
            .unwrap();
    }
    std::fs::write(dir.join("notes.txt"), vec![0u8; 5000]).unwrap();
    // keep the oldest: the next two oldest go, down to the limit
    let gone = cache::evict(&dir, std::slice::from_ref(&names[0]), 2000);
    assert_eq!(gone, 2);
    assert_eq!(ddc_files(&dir), vec![names[0].clone(), names[3].clone()]);
    assert!(
        dir.join("notes.txt").is_file(),
        "only our cache files are deleted"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cache_is_never_written_over_the_source() {
    let dir = tmp("safety");
    let key = cache::Key {
        hash: "b".repeat(64),
        importer: 7,
    };
    // a source that happens to carry the cache file name, inside the cache folder
    let src = dir.join(key.file_name());
    std::fs::write(&src, b"SOURCE").unwrap();
    let m = Model::open(":memory:").unwrap();
    let r = cache::write(&dir, &src, &key, &m, &[], &[], 0);
    assert!(r.is_err());
    assert_eq!(std::fs::read(&src).unwrap(), b"SOURCE");
    // and a cache folder elsewhere: written there only
    let cdir = dir.join("p.links");
    let src2 = dir.join("x.ifc");
    std::fs::write(&src2, b"SOURCE2").unwrap();
    assert!(cache::write(&cdir, &src2, &key, &m, &[], &[], 0).is_ok());
    assert_eq!(std::fs::read(&src2).unwrap(), b"SOURCE2");
    assert_eq!(ddc_files(&cdir), vec![key.file_name()]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_loads_are_deterministic_and_share_one_cache_file() {
    let dir = tmp("parallel");
    let src = dir.join("a.ifc");
    let segs: Vec<_> = (0..12)
        .map(|i| ((i as f64 * 2.0, 0.0), (i as f64 * 2.0 + 1.5, 0.0)))
        .collect();
    ifc_source(&src, &segs);
    let cdir = dir.join("p.links");
    let base = load_with(&src, None);
    let got: Vec<load::LinkData> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4)
            .map(|_| s.spawn(|| load_with(&src, Some(&cdir))))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for d in &got {
        assert_same(&base, d);
    }
    assert_eq!(ddc_files(&cdir).len(), 1, "one entry per source content");
    assert!(load_with(&src, Some(&cdir)).hit);
    let _ = std::fs::remove_dir_all(&dir);
}

fn cube(at: [f64; 3]) -> load::Elem {
    let mut pos = vec![];
    for k in 0..8 {
        pos.push([
            at[0] + (k & 1) as f64,
            at[1] + ((k >> 1) & 1) as f64,
            at[2] + ((k >> 2) & 1) as f64,
        ]);
    }
    load::Elem {
        kind: "mesh",
        id: 0,
        guid: String::new(),
        body: Body {
            pos,
            tri: vec![
                [0, 1, 2],
                [1, 3, 2],
                [4, 6, 5],
                [5, 6, 7],
                [0, 4, 1],
                [1, 4, 5],
                [2, 3, 6],
                [3, 7, 6],
                [0, 2, 4],
                [2, 6, 4],
                [1, 5, 3],
                [3, 5, 7],
            ],
        },
    }
}

#[test]
fn source_instances_repeats_and_keeps_far_origins_exact() {
    // georeferenced link: 500 km from its origin, three equal cubes and one other body
    let far = 500_000.0;
    let mut elems: Vec<load::Elem> = (0..3)
        .map(|i| cube([far + i as f64 * 10.0, 0.0, 0.0]))
        .collect();
    let mut odd = cube([far, 50.0, 0.0]);
    odd.body.pos[7][2] += 0.5;
    elems.push(odd);
    let s = source::build(&elems);
    assert_eq!(s.instanced, 3);
    assert_eq!(s.shapes.len(), 2);
    assert_eq!(s.shapes[0].places.len(), 3);
    let xf = Transform {
        dx: -far,
        dy: 2.0,
        ..Default::default()
    };
    let gs = source::place(&s, 9, &xf, 0.0);
    // the far corner lands at host (20+1, 3, 1) to well under a millimetre
    let g = &gs[0];
    let q = g.inst[2].m.transform_point3(g.proto.positions[7].into());
    assert!(
        (q.x - 21.0).abs() < 1e-4 && (q.y - 3.0).abs() < 1e-4 && (q.z - 1.0).abs() < 1e-4,
        "{q}"
    );
    // the same input gives the same source
    let again = source::build(&elems);
    assert_eq!(again.shapes.len(), s.shapes.len());
    assert!(again
        .shapes
        .iter()
        .zip(&s.shapes)
        .all(|(a, b)| a.places == b.places && a.mesh.positions == b.mesh.positions));
}

#[test]
fn host_edit_never_touches_link_render_sources() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    walls(&mut a.ed, &[((0.0, 0.0), (5.0, 0.0))]);
    let ctx = egui::Context::default();
    let mut st = std::mem::take(&mut a.links);
    apply(&mut st, &mut a, Act::Add("missing/site.ifc".into()));
    sync(&mut st, &mut a, &ctx);
    let id = st.list[0].id;
    st.jobs.clear();
    st.queue.clear();
    let segs: Vec<_> = (0..20)
        .map(|i| ((i as f64, 10.0), (i as f64 + 0.8, 10.0)))
        .collect();
    st.loaded.insert(id, Loaded::new(tests_link(&segs)));
    st.rev3d += 1;
    sync(&mut st, &mut a, &ctx);
    a.links = st;
    let rev3d = a.links.rev3d();
    let g1 = groups(&a.links, &a.ed);
    assert!(!g1.is_empty());
    let (b0, p0) = (source::builds(), source::placements());
    // host edits: new walls, each its own revision
    for k in 0..3 {
        walls(
            &mut a.ed,
            &[((0.0, 20.0 + k as f64), (4.0, 20.0 + k as f64))],
        );
        a.ed.revision += 1;
        let mut st = std::mem::take(&mut a.links);
        sync(&mut st, &mut a, &ctx);
        a.links = st;
        let host = ddc_render::Scene::new(vec![]);
        let sc = attach(&a.links, &a.ed, host);
        let g2 = &sc.instanced;
        assert_eq!(g2.len(), g1.len());
        assert!(g1.iter().zip(g2).all(|(x, y)| x.key == y.key
            && Arc::ptr_eq(&x.proto, &y.proto)
            && Arc::ptr_eq(&x.inst, &y.inst)));
    }
    assert_eq!(source::builds() - b0, 0, "0 link rebuilds on host edits");
    assert_eq!(source::placements() - p0, 0, "not even new matrices");
    assert_eq!(
        a.links.rev3d(),
        rev3d,
        "host edits leave the link set as is"
    );
    // moving the link writes matrices only: the shapes stay the same allocations
    let mut st = std::mem::take(&mut a.links);
    let moved = Transform {
        dx: 3.0,
        ..Default::default()
    };
    apply(&mut st, &mut a, Act::Update(id, moved, true));
    sync(&mut st, &mut a, &ctx);
    a.links = st;
    let g3 = groups(&a.links, &a.ed);
    assert!(g1
        .iter()
        .zip(&g3)
        .all(|(x, y)| Arc::ptr_eq(&x.proto, &y.proto) && !Arc::ptr_eq(&x.inst, &y.inst)));
    assert_eq!(source::builds() - b0, 0);
    assert_eq!(source::placements() - p0, 1);
}

fn tests_link(segs: &Segs) -> load::LinkData {
    let mut ed = Editor::new_in_memory().unwrap();
    walls(&mut ed, segs);
    let m = std::mem::replace(&mut ed.model, Model::open(":memory:").unwrap());
    load::build(m).unwrap()
}

#[test]
fn host_base_rebuilds_the_scene_without_composing_the_host() {
    let host = ddc_render::Scene::new(vec![ddc_render::SceneMesh {
        element: 1,
        tag: 0,
        positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
        triangles: vec![[0, 1, 2]],
    }]);
    let b = HostBase::of(&host);
    let again = b.scene();
    assert!(Arc::ptr_eq(&host.meshes[0], &again.meshes[0]));
    assert_eq!(host.bounds.map(|x| x.max), again.bounds.map(|x| x.max));
}

#[test]
fn big_link_plan_is_cut_off_the_ui_thread() {
    let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
    let ctx = egui::Context::default();
    let mut st = std::mem::take(&mut a.links);
    apply(&mut st, &mut a, Act::Add("missing/big.ifc".into()));
    sync(&mut st, &mut a, &ctx);
    let id = st.list[0].id;
    st.jobs.clear();
    st.queue.clear();
    // cubes standing across the cut height: more triangles than the inline limit
    let n = INLINE_PLAN_TRIS / 4 + 100;
    let elems: Vec<load::Elem> = (0..n)
        .map(|i| cube([(i % 100) as f64 * 2.0, (i / 100) as f64 * 2.0, 0.5]))
        .collect();
    st.loaded.insert(id, Loaded::new(load::finish(elems, vec![], 1)));
    sync(&mut st, &mut a, &ctx);
    let ld = &st.loaded[&id];
    assert!(ld.plan_job.is_some(), "big link: cut runs in the background");
    let mut tries = 0;
    while st.loaded[&id].key.is_none() && tries < 500 {
        std::thread::sleep(Duration::from_millis(10));
        sync(&mut st, &mut a, &ctx);
        tries += 1;
    }
    let ld = &st.loaded[&id];
    assert!(ld.key.is_some() && ld.plan_job.is_none());
    assert!(!ld.plan.is_empty(), "the lines arrive");
    a.links = st;
}
