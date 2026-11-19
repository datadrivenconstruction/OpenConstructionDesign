//! Mesh exchange at size: a 50 400-triangle model (4 200 cubes, one element each) goes out and
//! back through STL, OBJ, glTF binary and (with `dxf-write`) DXF, and is imported into a model.
//! Counts and boxes must survive; each step must stay inside a generous time budget.

use std::time::{Duration, Instant};

use ddc_io::gltf::{self, Scene};
use ddc_io::meshx::{self, MeshSet, Units};
use ddc_io::{obj, stl};
use ddc_model::Model;
use serde_json::json;

const SIDE: usize = 70;
const ROWS: usize = 60;
const TRIS: usize = SIDE * ROWS * 12;
/// Debug builds on a loaded machine; a release build is about ten times faster.
const BUDGET: Duration = Duration::from_secs(20);

fn cube(o: [f32; 3], s: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let p = (0..8u32)
        .map(|i| {
            [
                o[0] + s * (i & 1) as f32,
                o[1] + s * ((i >> 1) & 1) as f32,
                o[2] + s * ((i >> 2) & 1) as f32,
            ]
        })
        .collect();
    let t = vec![
        [0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4],
        [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5],
    ];
    (p, t)
}

fn big() -> MeshSet {
    let mut s = Scene::new("big");
    let mats = [
        s.material("concrete", [0.5, 0.5, 0.5, 1.0], json!(null)),
        s.material("brick", [0.6, 0.2, 0.1, 1.0], json!(null)),
    ];
    for i in 0..SIDE * ROWS {
        let (x, y) = ((i % SIDE) as f32 * 1.5, (i / SIDE) as f32 * 1.5);
        let (p, t) = cube([x, y, 0.0], 1.0);
        let extras = json!({"ddc": {"table": "wall", "id": i + 1, "category": "Walls"}});
        s.add_part(&format!("cube {i}"), mats[i % 2], &p, &t, extras).unwrap();
    }
    MeshSet::from_gltf(&s)
}

fn timed<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    let d = t.elapsed();
    eprintln!("{what}: {} ms", d.as_millis());
    assert!(d < BUDGET, "{what} took {d:?}");
    r
}

fn same_shape(what: &str, a: &MeshSet, b: &MeshSet, tol: f64) {
    assert_eq!(a.triangle_count(), b.triangle_count(), "{what}: triangles");
    let ((alo, ahi), (blo, bhi)) = (a.bbox().unwrap(), b.bbox().unwrap());
    for k in 0..3 {
        assert!(
            (alo[k] - blo[k]).abs() < tol && (ahi[k] - bhi[k]).abs() < tol,
            "{what}: box {alo:?} {ahi:?} vs {blo:?} {bhi:?}"
        );
    }
}

#[test]
fn fifty_thousand_triangles_round_trip_in_time() {
    let set = big();
    assert_eq!(set.triangle_count(), TRIS);
    assert_eq!(set.parts.len(), SIDE * ROWS);

    for u in [Units::Metre, Units::Millimetre] {
        let b = timed(&format!("stl write {u:?}"), || stl::write_binary(&set, u).unwrap());
        let (back, _) = timed(&format!("stl read {u:?}"), || stl::read_bytes(&b, "big.stl", u, None).unwrap());
        same_shape("stl", &set, &back, 1e-4);
    }

    let o = obj::ObjOptions::default();
    let (text, mtl) = timed("obj write", || obj::write(&set, "big.mtl", o).unwrap());
    let (back, w) = timed("obj read", || {
        obj::read_str(&text, "big.obj", o, &mut |_| Some(mtl.clone()), None).unwrap()
    });
    assert!(w.is_empty(), "{w:?}");
    same_shape("obj", &set, &back, 1e-6);
    assert_eq!(back.parts.len(), set.parts.len(), "one group per element");
    assert_eq!(back.parts[17].key, set.parts[17].key);
    assert_eq!(back.materials.len(), 2);

    let mut sc = Scene::new("again");
    let m = sc.material("m", [1.0, 1.0, 1.0, 1.0], json!(null));
    for p in &set.parts {
        let pos: Vec<[f32; 3]> = p.positions.iter().map(|v| v.map(|x| x as f32)).collect();
        sc.add_part(&p.name, m, &pos, &p.triangles, json!(null)).unwrap();
    }
    let glb = timed("glb write", || gltf::write_glb(&sc).unwrap());
    let (back, _) = timed("glb read", || gltf::read_bytes(&glb, "big.glb", None, None).unwrap());
    same_shape("glb", &set, &back, 1e-5);

    #[cfg(feature = "dxf-write")]
    {
        use ddc_io::mesh_dxf;
        let opt = mesh_dxf::DxfOptions::standard(&set, &[("L1".into(), 0.0)], Units::Millimetre);
        let (txt, st) = timed("dxf write", || mesh_dxf::write_string(&set, &opt).unwrap());
        assert_eq!(st.faces, TRIS);
        let doc = timed("dxf read", || ddc_io::dxf::read_str(&txt).unwrap());
        assert!(doc.entities.len() >= TRIS, "{} entities", doc.entities.len());
    }

    let mut model = Model::open(":memory:").unwrap();
    model.add_level("L1", 0.0).unwrap();
    let st = timed("import into model", || meshx::import_into(&mut model, &set, "big.stl", None).unwrap());
    assert_eq!(st.meshes, SIDE * ROWS);
    assert_eq!(st.triangles, TRIS);
}
