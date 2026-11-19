use super::*;
use crate::gltf::{json, write_glb, write_gltf, Scene};
use crate::meshx::MeshSet;

fn close(a: [f64; 3], b: [f64; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
}

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

fn scene() -> Scene {
    let mut s = Scene::new("test");
    let a = s.material("concrete", [0.5, 0.25, 0.125, 1.0], json!(null));
    let b = s.material("glass", [0.25, 0.5, 0.75, 0.5], json!(null));
    let (p, t) = cube([1.0, 2.0, 3.0], 2.0);
    s.add_part(
        "wall A",
        a,
        &p,
        &t,
        json!({"ddc": {"table": "wall", "id": 7, "category": "wall", "level": "L1"}}),
    )
    .unwrap();
    let (p, t) = cube([0.0, 0.0, 0.0], 1.0);
    let mut m1 = [0.0f32; 16];
    let mut m2 = [0.0f32; 16];
    for (m, at) in [(&mut m1, [5.0, 0.0, 0.0]), (&mut m2, [-3.0, 4.0, 1.0])] {
        m[0] = 1.0;
        m[5] = 1.0;
        m[10] = 1.0;
        m[15] = 1.0;
        m[12..15].copy_from_slice(&at);
    }
    let n = s.add_instances(
        "window",
        b,
        &p,
        &t,
        &[
            (m1, json!({"ddc": {"table": "comp", "id": 11}})),
            (m2, json!({"ddc": {"table": "comp", "id": 12}})),
        ],
    );
    assert_eq!(n, 2);
    s
}

fn check_roundtrip(set: &MeshSet, warns: &[String]) {
    let s = scene();
    let want = MeshSet::from_gltf(&s);
    assert!(warns.is_empty(), "{warns:?}");
    assert_eq!(set.triangle_count(), want.triangle_count());
    let (lo, hi) = set.bbox().unwrap();
    let (wlo, whi) = want.bbox().unwrap();
    assert!(close(lo, wlo) && close(hi, whi), "{lo:?} {hi:?} vs {wlo:?} {whi:?}");
    let keys: Vec<&str> = set.parts.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["wall:7", "comp:11", "comp:12"]);
    assert_eq!(set.parts[0].name, "wall A");
    assert_eq!(set.parts[1].name, "window");
    assert_eq!(set.parts[0].category, "wall");
    assert_eq!(set.parts[1].category, GENERIC);
    assert_eq!(set.parts[0].level.as_deref(), Some("L1"));
    assert_eq!(set.materials.len(), 2);
    assert_eq!(set.materials[set.parts[0].material.unwrap()].rgba, [0.5, 0.25, 0.125, 1.0]);
    assert_eq!(set.materials[set.parts[1].material.unwrap()].rgba, [0.25, 0.5, 0.75, 0.5]);
    let b0 = set.parts[0].bbox().unwrap();
    assert!(close(b0.0, [1.0, 2.0, 3.0]) && close(b0.1, [3.0, 4.0, 5.0]));
}

#[test]
fn roundtrip_through_the_own_writer() {
    let glb = write_glb(&scene()).unwrap();
    let (set, warns) = read_bytes(&glb, "x.glb", None, None).unwrap();
    check_roundtrip(&set, &warns);
    let txt = write_gltf(&scene()).unwrap();
    let (set, warns) = read_bytes(txt.as_bytes(), "x.gltf", None, None).unwrap();
    check_roundtrip(&set, &warns);
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/gltf/two_nodes.gltf")
}

#[test]
fn hand_written_fixture_has_exact_positions() {
    let (set, warns) = read_file(&fixture(), None).unwrap();
    assert!(warns.is_empty(), "{warns:?}");
    assert_eq!(set.parts.len(), 2);
    let (strip, tri) = (&set.parts[0], &set.parts[1]);
    assert_eq!((strip.name.as_str(), strip.key.as_str()), ("strip", "gltf:1"));
    assert_eq!((tri.name.as_str(), tri.key.as_str()), ("tri", "gltf:2"));
    let want = [[12.0, 0.0, 0.0], [12.0, 2.0, 0.0], [12.0, 0.0, 2.0], [12.0, 2.0, 2.0]];
    assert_eq!(strip.positions.len(), 4);
    for (g, w) in strip.positions.iter().zip(want) {
        assert!(close(*g, w), "{g:?} vs {w:?}");
    }
    assert_eq!(strip.triangles, [[0, 1, 2], [2, 1, 3]]);
    let want = [[0.0, 0.0, 5.0], [1.0, 0.0, 5.0], [0.0, 0.0, 6.0]];
    for (g, w) in tri.positions.iter().zip(want) {
        assert!(close(*g, w), "{g:?} vs {w:?}");
    }
    assert_eq!(tri.triangles, [[0, 1, 2]]);
    assert_eq!(set.materials[strip.material.unwrap()].name, "red");
    assert_eq!(set.materials[tri.material.unwrap()].rgba, [0.1, 0.2, 0.3, 0.5]);
}

/// One quad (4 vertices) and 6 u8 indices; tests tweak the JSON.
fn quad() -> (Value, Vec<u8>) {
    let mut bin = vec![];
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]] {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    bin.extend_from_slice(&[0, 1, 2, 0, 2, 3, 0, 0]);
    let j = json!({
        "asset": {"version": "2.0"},
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 1, "componentType": 5121, "count": 6, "type": "SCALAR"}],
        "bufferViews": [{"buffer": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 6}],
    });
    (j, bin)
}

fn pack(mut j: Value, bin: &[u8]) -> Vec<u8> {
    let uri = format!("data:;base64,{}", crate::bridge::sig::b64_encode(bin));
    j["buffers"] = json!([{"byteLength": bin.len(), "uri": uri}]);
    serde_json::to_vec(&j).unwrap()
}

fn read(j: Value, bin: &[u8]) -> Result<(MeshSet, Vec<String>), String> {
    read_bytes(&pack(j, bin), "t.gltf", None, None)
}

fn normal(p: &Part, t: [u32; 3]) -> [f64; 3] {
    let v = |i: u32| p.positions[i as usize];
    let (a, b, c) = (v(t[0]), v(t[1]), v(t[2]));
    let (u, w) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
    [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]]
}

#[test]
fn primitive_modes_and_index_types() {
    let (mut j, bin) = quad();
    let (set, _) = read(j.clone(), &bin).unwrap();
    assert_eq!(set.triangle_count(), 1, "plain triangles drop the remainder");
    j["meshes"][0]["primitives"][0]["indices"] = json!(1);
    let (set, _) = read(j.clone(), &bin).unwrap();
    assert_eq!(set.parts[0].triangles, [[0, 1, 2], [0, 2, 3]]);
    j["meshes"][0]["primitives"][0]["mode"] = json!(6);
    let (set, _) = read(j.clone(), &bin).unwrap();
    assert_eq!(set.parts[0].triangles.len(), 4);
    j["meshes"][0]["primitives"][0].as_object_mut().unwrap().remove("indices");
    j["meshes"][0]["primitives"][0]["mode"] = json!(6);
    let (set, _) = read(j.clone(), &bin).unwrap();
    assert_eq!(set.parts[0].triangles, [[0, 1, 2], [0, 2, 3]]);
    j["meshes"][0]["primitives"][0]["mode"] = json!(5);
    let (set, _) = read(j, &bin).unwrap();
    assert_eq!(set.parts[0].triangles, [[0, 1, 2], [2, 1, 3]]);
}

#[test]
fn lines_and_sparse_accessors_warn() {
    let (mut j, bin) = quad();
    let good = json!({"attributes": {"POSITION": 0}, "mode": 6});
    j["meshes"][0]["primitives"] = json!([
        {"attributes": {"POSITION": 0}, "mode": 1},
        {"attributes": {"POSITION": 2}},
        good]);
    j["accessors"].as_array_mut().unwrap().push(json!(
        {"componentType": 5126, "count": 4, "type": "VEC3", "sparse": {}}));
    let (set, warns) = read(j, &bin).unwrap();
    assert_eq!(set.parts.len(), 1);
    assert_eq!(set.parts[0].key, "gltf:0#2");
    assert!(warns.iter().any(|w| w.contains("point or line")), "{warns:?}");
    assert!(warns.iter().any(|w| w.contains("sparse")), "{warns:?}");
}

#[test]
fn negative_determinant_keeps_the_outside() {
    let (mut j, bin) = quad();
    j["meshes"][0]["primitives"][0]["indices"] = json!(1);
    let (set, _) = read(j.clone(), &bin).unwrap();
    let n0 = normal(&set.parts[0], set.parts[0].triangles[0]);
    j["nodes"][0]["scale"] = json!([-1.0, 1.0, 1.0]);
    let (set, _) = read(j, &bin).unwrap();
    let n1 = normal(&set.parts[0], set.parts[0].triangles[0]);
    assert!(n0[1] < 0.0 && n1[1] < 0.0, "{n0:?} {n1:?}");
}

#[test]
fn spec_gloss_colour_and_fallback_names() {
    let (mut j, bin) = quad();
    j["materials"] = json!([{"extensions": {"KHR_materials_pbrSpecularGlossiness":
        {"diffuseFactor": [0.2, 0.4, 0.6, 1.0]}}}]);
    j["meshes"][0]["name"] = json!("shape");
    j["meshes"][0]["primitives"][0]["material"] = json!(0);
    let (set, _) = read(j, &bin).unwrap();
    assert_eq!(set.parts[0].name, "shape");
    assert_eq!(set.materials[0].name, "material 0");
    assert_eq!(set.materials[0].rgba, [0.2, 0.4, 0.6, 1.0]);
}

#[test]
fn external_buffer_next_to_the_file() {
    let (mut j, bin) = quad();
    let dir = std::env::temp_dir().join(format!("ddc_gltf_read_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("quad data.bin"), &bin).unwrap();
    j["buffers"] = json!([{"byteLength": bin.len(), "uri": "quad%20data.bin"}]);
    let path = dir.join("q.gltf");
    std::fs::write(&path, serde_json::to_vec(&j).unwrap()).unwrap();
    let (set, _) = read_file(&path, None).unwrap();
    assert_eq!(set.triangle_count(), 1);
    j["buffers"][0]["uri"] = json!("../quad.bin");
    let e = read_bytes(&serde_json::to_vec(&j).unwrap(), "q.gltf", Some(&dir), None).unwrap_err();
    assert!(e.contains("relative path") && e.contains(".."), "{e}");
    for bad in ["/etc/x.bin", "C:/x.bin", "sub/../../x.bin", "http://h/x.bin"] {
        j["buffers"][0]["uri"] = json!(bad);
        let e = read_bytes(&serde_json::to_vec(&j).unwrap(), "q.gltf", Some(&dir), None).unwrap_err();
        assert!(e.contains("relative path"), "{bad}: {e}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn bad_input_gives_clear_errors() {
    let glb = write_glb(&scene()).unwrap();
    for cut in [5, 19, 40, glb.len() - 10] {
        let e = read_bytes(&glb[..cut], "x.glb", None, None).unwrap_err();
        assert!(e.starts_with("x.glb: ") && e.contains("truncated"), "{cut}: {e}");
    }
    let (mut j, bin) = quad();
    j["meshes"][0]["primitives"][0]["indices"] = json!(1);
    let mut bad = bin.clone();
    bad[50] = 9;
    let e = read(j.clone(), &bad).unwrap_err();
    assert!(e.contains("accessor 1") && e.contains("out of range"), "{e}");
    j["accessors"][1]["count"] = json!(40);
    let e = read(j.clone(), &bin).unwrap_err();
    assert!(e.contains("accessor 1") && e.contains("do not fit"), "{e}");
    j["accessors"][1]["count"] = json!(6);
    j["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]);
    let e = read(j, &bin).unwrap_err();
    assert!(e.contains("KHR_draco_mesh_compression"), "{e}");
    let e = read_bytes(b"{\n  \"asset\": ", "a.gltf", None, None).unwrap_err();
    assert!(e.contains("line 2") && e.contains("column"), "{e}");
    let e = read_bytes(b"{}", "a.gltf", None, None).unwrap_err();
    assert!(e.contains("2.0"), "{e}");
}

#[test]
fn cancel_before_and_during_parse() {
    let glb = write_glb(&scene()).unwrap();
    let p = Progress::new();
    p.cancel();
    let r = read_bytes(&glb, "x.glb", None, Some(&*p));
    assert_eq!(r.unwrap_err(), CANCELLED);
    let p = Progress::new();
    assert!(read_bytes(&glb, "x.glb", None, Some(&*p)).is_ok());
    assert_eq!(p.counts().1, 4);
}
