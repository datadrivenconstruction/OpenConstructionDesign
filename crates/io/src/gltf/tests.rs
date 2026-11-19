// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Round trip: write a scene, parse the .glb with a small independent validator, compare.

use super::*;
use serde_json::{json, Value};

/// Cube corner `o`, side `s`, outward faces.
fn cube(o: [f32; 3], s: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let mut p = vec![];
    for i in 0..8u32 {
        p.push([
            o[0] + s * (i & 1) as f32,
            o[1] + s * ((i >> 1) & 1) as f32,
            o[2] + s * ((i >> 2) & 1) as f32,
        ]);
    }
    let t = vec![
        [0, 2, 1],
        [1, 2, 3],
        [4, 5, 6],
        [5, 7, 6],
        [0, 1, 4],
        [1, 5, 4],
        [2, 6, 3],
        [3, 6, 7],
        [0, 4, 2],
        [2, 4, 6],
        [1, 3, 5],
        [3, 7, 5],
    ];
    (p, t)
}

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn parse(bytes: &[u8]) -> Result<Glb, String> {
    if bytes.len() < 20 || &bytes[0..4] != b"glTF" || u32_at(bytes, 4) != 2 {
        return Err("bad header".into());
    }
    if u32_at(bytes, 8) as usize != bytes.len() || bytes.len() % 4 != 0 {
        return Err("bad total length".into());
    }
    let (mut at, mut json, mut bin) = (12, None, vec![]);
    while at < bytes.len() {
        let len = u32_at(bytes, at) as usize;
        let ty = u32_at(bytes, at + 4);
        let body = bytes
            .get(at + 8..at + 8 + len)
            .ok_or("chunk past the end")?;
        if len % 4 != 0 {
            return Err("chunk not 4-aligned".into());
        }
        match ty {
            0x4E4F_534A if json.is_none() => {
                json = Some(serde_json::from_slice::<Value>(body).map_err(|e| e.to_string())?)
            }
            0x004E_4942 => bin = body.to_vec(),
            _ => return Err(format!("unexpected chunk {ty:#x}")),
        }
        at += 8 + len;
    }
    Ok(Glb {
        json: json.ok_or("no JSON chunk")?,
        bin,
    })
}

fn arr<'a>(j: &'a Value, k: &str) -> &'a [Value] {
    j.get(k)
        .and_then(Value::as_array)
        .map_or(&[], |v| v.as_slice())
}

fn idx(v: &Value, k: &str) -> usize {
    v[k].as_u64()
        .unwrap_or_else(|| panic!("missing {k} in {v}")) as usize
}

/// Structural checks of the glTF 2.0 rules the writer relies on.
fn validate(g: &Glb) -> Result<(), String> {
    let j = &g.json;
    if j["asset"]["version"] != "2.0" {
        return Err("asset.version".into());
    }
    let buf_len = j["buffers"][0]["byteLength"].as_u64().unwrap_or(0) as usize;
    if buf_len > g.bin.len() || g.bin.len() - buf_len > 3 {
        return Err("buffer length vs BIN chunk".into());
    }
    for v in arr(j, "bufferViews") {
        if idx(v, "byteOffset") % 4 != 0 || idx(v, "byteOffset") + idx(v, "byteLength") > buf_len {
            return Err("bufferView range".into());
        }
    }
    for (i, a) in arr(j, "accessors").iter().enumerate() {
        let view = &arr(j, "bufferViews")[idx(a, "bufferView")];
        let (comp, n) = match a["type"].as_str() {
            Some("VEC3") => (12, 3),
            Some("SCALAR") => (4, 1),
            t => return Err(format!("accessor type {t:?}")),
        };
        let ct = idx(a, "componentType");
        if !(ct == 5126 && n == 3 || ct == 5125 && n == 1) {
            return Err(format!("accessor {i} component type"));
        }
        if idx(a, "count") * comp > idx(view, "byteLength") {
            return Err(format!("accessor {i} overruns its view"));
        }
    }
    for (mi, m) in arr(j, "meshes").iter().enumerate() {
        for p in arr(m, "primitives") {
            let pos = &arr(j, "accessors")[idx(&p["attributes"], "POSITION")];
            let ind = &arr(j, "accessors")[idx(p, "indices")];
            if idx(ind, "count") % 3 != 0 || p["mode"] != 4 {
                return Err(format!("mesh {mi} is not a triangle list"));
            }
            if idx(p, "material") >= arr(j, "materials").len() {
                return Err(format!("mesh {mi} material"));
            }
            let (pv, iv) = (read_pos(g, pos), read_idx(g, ind));
            if iv.iter().any(|&i| i as usize >= pv.len()) {
                return Err(format!("mesh {mi} index out of range"));
            }
            for k in 0..3 {
                let lo = pv.iter().map(|p| p[k]).fold(f32::INFINITY, f32::min);
                let hi = pv.iter().map(|p| p[k]).fold(f32::NEG_INFINITY, f32::max);
                let (a, b) = (
                    pos["min"][k].as_f64().unwrap(),
                    pos["max"][k].as_f64().unwrap(),
                );
                if (a - f64::from(lo)).abs() > 1e-6 || (b - f64::from(hi)).abs() > 1e-6 {
                    return Err(format!("mesh {mi} min/max"));
                }
            }
        }
    }
    let n = arr(j, "nodes").len();
    for (i, node) in arr(j, "nodes").iter().enumerate() {
        if let Some(m) = node.get("mesh") {
            if m.as_u64().unwrap() as usize >= arr(j, "meshes").len() {
                return Err(format!("node {i} mesh"));
            }
        }
        for c in arr(node, "children") {
            let c = c.as_u64().unwrap() as usize;
            if c >= n || c <= i {
                return Err(format!("node {i} child {c}"));
            }
        }
    }
    Ok(())
}

fn view_bytes<'a>(g: &'a Glb, acc: &Value) -> &'a [u8] {
    let v = &arr(&g.json, "bufferViews")[idx(acc, "bufferView")];
    let at = idx(v, "byteOffset");
    &g.bin[at..at + idx(v, "byteLength")]
}

fn read_pos(g: &Glb, acc: &Value) -> Vec<[f32; 3]> {
    let b = view_bytes(g, acc);
    (0..idx(acc, "count"))
        .map(|i| {
            let f = |k: usize| {
                f32::from_le_bytes(b[i * 12 + k * 4..i * 12 + k * 4 + 4].try_into().unwrap())
            };
            [f(0), f(1), f(2)]
        })
        .collect()
}

fn read_idx(g: &Glb, acc: &Value) -> Vec<u32> {
    let b = view_bytes(g, acc);
    (0..idx(acc, "count")).map(|i| u32_at(b, i * 4)).collect()
}

/// World position (glTF axes) of vertex `v` of node `n`: translation, then the root's turn.
fn world(g: &Glb, node: &Value, v: [f32; 3]) -> [f32; 3] {
    let t = node.get("translation").map_or([0.0; 3], |t| {
        [0, 1, 2].map(|k| t[k].as_f64().unwrap() as f32)
    });
    let m = [v[0] + t[0], v[1] + t[1], v[2] + t[2]];
    let root = &arr(&g.json, "nodes")[0];
    assert!((root["rotation"][0].as_f64().unwrap() + 0.7071067811865476).abs() < 1e-9);
    [m[0], m[2], -m[1]]
}

fn mesh_of(g: &Glb, node: &Value) -> (Vec<[f32; 3]>, Vec<u32>) {
    let m = &arr(&g.json, "meshes")[idx(node, "mesh")];
    let p = &m["primitives"][0];
    (
        read_pos(
            g,
            &arr(&g.json, "accessors")[idx(&p["attributes"], "POSITION")],
        ),
        read_idx(g, &arr(&g.json, "accessors")[idx(p, "indices")]),
    )
}

fn sample() -> Scene {
    let mut s = Scene::new("test");
    let wall = s.material("wall", [0.8, 0.8, 0.78, 1.0], json!({"category": "wall"}));
    let glass = s.material("window", [0.6, 0.8, 0.9, 0.35], Value::Null);
    for (i, x) in [0.0f32, 4.0, 8.0].into_iter().enumerate() {
        let (p, t) = cube([x, 1.0, 0.5], 2.0);
        s.add_part(
            &format!("wall {i}"),
            wall,
            &p,
            &t,
            json!({"ddc": {"element": i, "category": "wall", "props": {"Mark": "W1", "Fire": 60}}}),
        )
        .unwrap();
    }
    let (p, t) = cube([20.0, 0.0, 0.0], 1.0);
    s.add_part("window", glass, &p, &t, json!({"ddc": {"element": 9}}))
        .unwrap();
    s
}

#[test]
fn round_trip_parses_and_matches() {
    let s = sample();
    let bytes = write_glb(&s).unwrap();
    let g = parse(&bytes).unwrap();
    validate(&g).unwrap();
    let nodes = arr(&g.json, "nodes");
    assert_eq!(nodes.len(), 5, "root + 4 parts");
    assert_eq!(arr(&g.json, "materials").len(), 2);
    assert_eq!(g.json["materials"][1]["alphaMode"], "BLEND");
    assert_eq!(g.json["materials"][0]["alphaMode"], "OPAQUE");
    // extras: ids and properties survive
    assert_eq!(nodes[2]["extras"]["ddc"]["element"], 1);
    assert_eq!(nodes[2]["extras"]["ddc"]["props"]["Mark"], "W1");
    assert_eq!(nodes[3]["extras"]["ddc"]["props"]["Fire"], 60);
    // every vertex lands where the model put it, turned to Y-up
    for (k, node) in nodes[1..].iter().enumerate() {
        let src = &s.nodes[k];
        let (p, _) = mesh_of(&g, node);
        for (v, want) in p.iter().zip(&s.meshes[src.mesh].positions) {
            let w = world(&g, node, *v);
            let m = [
                want[0] + src.translation[0],
                want[1] + src.translation[1],
                want[2] + src.translation[2],
            ];
            for (a, b) in w.iter().zip([m[0], m[2], -m[1]]) {
                assert!((a - b).abs() < 1e-5, "{w:?} vs {m:?}");
            }
        }
    }
}

#[test]
fn repeated_parts_share_one_mesh() {
    let s = sample();
    // three equal cubes at different offsets: one mesh, plus the window cube (other material)
    assert_eq!(s.meshes.len(), 2);
    assert_eq!(s.nodes.len(), 4);
    assert_eq!(s.nodes[0].mesh, s.nodes[2].mesh);
    assert_ne!(s.nodes[0].mesh, s.nodes[3].mesh);
    let g = parse(&write_glb(&s).unwrap()).unwrap();
    assert_eq!(arr(&g.json, "meshes").len(), 2);
    // a different size is a different mesh
    let mut s = Scene::new("t");
    let m = s.material("m", [1.0; 4], Value::Null);
    let (p, t) = cube([0.0; 3], 1.0);
    let (p2, t2) = cube([5.0; 3], 1.5);
    s.add_part("a", m, &p, &t, Value::Null).unwrap();
    s.add_part("b", m, &p2, &t2, Value::Null).unwrap();
    assert_eq!(s.meshes.len(), 2);
}

#[test]
fn explicit_instances_use_matrices() {
    let mut s = Scene::new("t");
    let m = s.material("mep", [0.2, 0.4, 0.9, 1.0], Value::Null);
    let (p, t) = cube([0.0; 3], 0.5);
    let mut a = [0.0f32; 16];
    a[0] = 1.0;
    a[5] = 1.0;
    a[10] = 1.0;
    a[15] = 1.0;
    let mut b = a;
    b[12] = 3.0;
    let n = s.add_instances(
        "fixture",
        m,
        &p,
        &t,
        &[(a, json!({"id": 1})), (b, json!({"id": 2}))],
    );
    assert_eq!(n, 2);
    assert_eq!(s.meshes.len(), 1);
    let g = parse(&write_glb(&s).unwrap()).unwrap();
    validate(&g).unwrap();
    assert_eq!(g.json["nodes"][2]["matrix"][12], 3.0);
    assert_eq!(g.json["nodes"][2]["extras"]["id"], 2);
}

#[test]
fn invalid_input_is_refused_and_empty_scene_is_valid() {
    let mut s = Scene::new("t");
    let m = s.material("m", [1.0; 4], Value::Null);
    assert!(s.add_part("x", m, &[], &[], Value::Null).is_none());
    assert!(s
        .add_part("x", m, &[[0.0; 3]], &[[0, 1, 2]], Value::Null)
        .is_none());
    assert!(s
        .add_part("x", m, &[[f32::NAN, 0.0, 0.0]], &[[0, 0, 0]], Value::Null)
        .is_none());
    assert!(s
        .add_part("x", 7, &[[0.0; 3]], &[[0, 0, 0]], Value::Null)
        .is_none());
    let g = parse(&write_glb(&Scene::new("empty")).unwrap()).unwrap();
    validate(&g).unwrap();
    assert!(g.bin.is_empty());
}

#[test]
fn validator_catches_a_broken_file() {
    let mut bytes = write_glb(&sample()).unwrap();
    bytes[8] ^= 1;
    assert!(parse(&bytes).is_err());
    let mut g = parse(&write_glb(&sample()).unwrap()).unwrap();
    g.json["nodes"][1]["mesh"] = json!(99);
    assert!(validate(&g).is_err());
}

#[test]
fn gltf_text_embeds_the_same_buffer() {
    let g = parse(&write_glb(&sample()).unwrap()).unwrap();
    let text: Value = serde_json::from_str(&write_gltf(&sample()).unwrap()).unwrap();
    let uri = text["buffers"][0]["uri"].as_str().unwrap();
    let b64 = uri.strip_prefix("data:application/octet-stream;base64,").unwrap();
    let bin = crate::bridge::sig::b64(b64).unwrap();
    let n = g.json["buffers"][0]["byteLength"].as_u64().unwrap() as usize;
    assert_eq!(bin.len(), n, "exactly the stated length");
    assert_eq!(&bin[..], &g.bin[..n]);
    // the same document otherwise: the glb validator accepts it with that buffer
    let mut same = text.clone();
    same["buffers"][0].as_object_mut().unwrap().remove("uri");
    assert_eq!(same, g.json);
    validate(&Glb { json: same, bin: g.bin.clone() }).unwrap();
}

#[test]
fn file_is_written() {
    let dir = std::env::temp_dir().join(format!("ddc_gltf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("a.glb");
    write_glb_file(&sample(), &p).unwrap();
    let g = parse(&std::fs::read(&p).unwrap()).unwrap();
    validate(&g).unwrap();
    std::fs::remove_dir_all(&dir).ok();
}
