// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! glTF 2.0 reader (.glb or .gltf with embedded or neighbouring buffers): triangles of the
//! default scene with node transforms, grouped by material name. Metres, y up.

use std::path::Path;

use base64::Engine;
use serde_json::Value;

use super::{Axes, Collect, Source};
use ddc_geom::math::V3;

type M4 = [f64; 16];

const ID: M4 = [
    1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.,
];

/// Column-major product a·b.
fn mul(a: &M4, b: &M4) -> M4 {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

fn apply(m: &M4, p: [f64; 3]) -> [f64; 3] {
    let r = |i: usize| m[i] * p[0] + m[4 + i] * p[1] + m[8 + i] * p[2] + m[12 + i];
    [r(0), r(1), r(2)]
}

fn det3(m: &M4) -> f64 {
    m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2])
}

fn nums(v: &Value) -> Vec<f64> {
    v.as_array()
        .map(|a| a.iter().filter_map(Value::as_f64).collect())
        .unwrap_or_default()
}

fn local(n: &Value) -> M4 {
    let m = nums(&n["matrix"]);
    if m.len() == 16 {
        let mut o = [0.0; 16];
        o.copy_from_slice(&m);
        return o;
    }
    let t = nums(&n["translation"]);
    let r = nums(&n["rotation"]);
    let s = nums(&n["scale"]);
    let (tx, ty, tz) = if t.len() == 3 { (t[0], t[1], t[2]) } else { (0., 0., 0.) };
    let (x, y, z, w) = if r.len() == 4 { (r[0], r[1], r[2], r[3]) } else { (0., 0., 0., 1.) };
    let (sx, sy, sz) = if s.len() == 3 { (s[0], s[1], s[2]) } else { (1., 1., 1.) };
    [
        (1. - 2. * (y * y + z * z)) * sx,
        (2. * (x * y + z * w)) * sx,
        (2. * (x * z - y * w)) * sx,
        0.,
        (2. * (x * y - z * w)) * sy,
        (1. - 2. * (x * x + z * z)) * sy,
        (2. * (y * z + x * w)) * sy,
        0.,
        (2. * (x * z + y * w)) * sz,
        (2. * (y * z - x * w)) * sz,
        (1. - 2. * (x * x + y * y)) * sz,
        0.,
        tx,
        ty,
        tz,
        1.,
    ]
}

struct Doc {
    j: Value,
    bufs: Vec<Vec<u8>>,
}

impl Doc {
    fn view(&self, acc: &Value) -> Result<(&[u8], usize, usize), String> {
        if acc.get("sparse").is_some() {
            return Err("sparse accessors are not supported".into());
        }
        let bv = &self.j["bufferViews"][acc["bufferView"].as_u64().ok_or("accessor without data")? as usize];
        let buf = self
            .bufs
            .get(bv["buffer"].as_u64().unwrap_or(0) as usize)
            .ok_or("missing buffer")?;
        let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize
            + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = bv["byteLength"].as_u64().ok_or("bad buffer view")? as usize;
        let stride = bv["byteStride"].as_u64().unwrap_or(0) as usize;
        let end = (bv["byteOffset"].as_u64().unwrap_or(0) as usize)
            .checked_add(len)
            .filter(|e| *e <= buf.len())
            .ok_or("buffer view outside its buffer")?;
        Ok((&buf[..end], off, stride))
    }
    fn positions(&self, i: usize) -> Result<Vec<[f64; 3]>, String> {
        let acc = &self.j["accessors"][i];
        if acc["componentType"].as_u64() != Some(5126) || acc["type"].as_str() != Some("VEC3") {
            return Err("positions must be float VEC3".into());
        }
        let n = acc["count"].as_u64().ok_or("bad accessor")? as usize;
        let (b, off, stride) = self.view(acc)?;
        let stride = if stride == 0 { 12 } else { stride };
        let mut out = Vec::with_capacity(n);
        for k in 0..n {
            let at = off + k * stride;
            let f = |j: usize| -> Result<f64, String> {
                let s = b.get(at + 4 * j..at + 4 * j + 4).ok_or("accessor outside its view")?;
                Ok(f32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f64)
            };
            out.push([f(0)?, f(1)?, f(2)?]);
        }
        Ok(out)
    }
    fn indices(&self, i: usize) -> Result<Vec<u32>, String> {
        let acc = &self.j["accessors"][i];
        let n = acc["count"].as_u64().ok_or("bad accessor")? as usize;
        let size = match acc["componentType"].as_u64() {
            Some(5121) => 1,
            Some(5123) => 2,
            Some(5125) => 4,
            _ => return Err("bad index type".into()),
        };
        let (b, off, stride) = self.view(acc)?;
        let stride = if stride == 0 { size } else { stride };
        (0..n)
            .map(|k| {
                let at = off + k * stride;
                let s = b.get(at..at + size).ok_or("accessor outside its view")?;
                Ok(match size {
                    1 => s[0] as u32,
                    2 => u16::from_le_bytes([s[0], s[1]]) as u32,
                    _ => u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                })
            })
            .collect()
    }
}

fn load_uri(uri: &str, dir: Option<&Path>) -> Result<Vec<u8>, String> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (_, data) = rest.split_once(";base64,").ok_or("unsupported data URI")?;
        return base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| format!("data URI: {e}"));
    }
    if uri.contains("..") || uri.contains(':') || uri.starts_with('/') {
        return Err(format!("buffer outside the model folder: {uri}"));
    }
    let dir = dir.ok_or("external buffer without a folder")?;
    let p = dir.join(uri.replace("%20", " "));
    std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Source, String> {
    let (json, bin): (&[u8], Option<Vec<u8>>) = if bytes.starts_with(b"glTF") {
        let u = |o: usize| -> Result<u32, String> {
            let s = bytes.get(o..o + 4).ok_or("truncated .glb")?;
            Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        };
        let jl = u(12)? as usize;
        let json = bytes.get(20..20 + jl).ok_or("truncated .glb")?;
        let at = 20 + jl;
        let bin = if bytes.len() >= at + 8 {
            let bl = u(at)? as usize;
            Some(bytes.get(at + 8..at + 8 + bl).ok_or("truncated .glb")?.to_vec())
        } else {
            None
        };
        (json, bin)
    } else {
        (bytes, None)
    };
    let j: Value = serde_json::from_slice(json).map_err(|e| format!("glTF JSON: {e}"))?;
    let mut bufs = vec![];
    for (k, b) in j["buffers"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        match b["uri"].as_str() {
            Some(u) => bufs.push(load_uri(u, dir)?),
            None if k == 0 => bufs.push(bin.clone().ok_or("buffer without data")?),
            None => return Err("buffer without data".into()),
        }
    }
    let doc = Doc { j, bufs };
    let j = &doc.j;
    let mut out = Source::default();
    let mut c = Collect::default();
    let scene = j["scene"].as_u64().unwrap_or(0) as usize;
    let mut stack: Vec<(usize, M4, usize)> = j["scenes"][scene]["nodes"]
        .as_array()
        .map(|a| a.iter().filter_map(|n| n.as_u64()).map(|n| (n as usize, ID, 0)).collect())
        .unwrap_or_default();
    if out.name.is_empty() {
        out.name = j["scenes"][scene]["name"].as_str().unwrap_or("").to_string();
    }
    let mut skipped = 0usize;
    while let Some((n, parent, depth)) = stack.pop() {
        if depth > 64 {
            return Err("node hierarchy too deep".into());
        }
        let node = &j["nodes"][n];
        let m = mul(&parent, &local(node));
        for ch in node["children"].as_array().into_iter().flatten() {
            if let Some(c) = ch.as_u64() {
                stack.push((c as usize, m, depth + 1));
            }
        }
        let Some(mi) = node["mesh"].as_u64() else { continue };
        for prim in j["meshes"][mi as usize]["primitives"].as_array().into_iter().flatten() {
            let mode = prim["mode"].as_u64().unwrap_or(4);
            let Some(pa) = prim["attributes"]["POSITION"].as_u64() else {
                skipped += 1;
                continue;
            };
            let pos: Vec<V3> = doc
                .positions(pa as usize)?
                .into_iter()
                .map(|p| Axes::GLTF.map(apply(&m, p)))
                .collect();
            let idx = match prim["indices"].as_u64() {
                Some(i) => doc.indices(i as usize)?,
                None => (0..pos.len() as u32).collect(),
            };
            if idx.iter().any(|&i| i as usize >= pos.len()) {
                return Err("index out of range".into());
            }
            let mut tris: Vec<[u32; 3]> = match mode {
                4 => idx.chunks_exact(3).map(|t| [t[0], t[1], t[2]]).collect(),
                5 => (2..idx.len())
                    .map(|k| {
                        if k % 2 == 0 {
                            [idx[k - 2], idx[k - 1], idx[k]]
                        } else {
                            [idx[k - 1], idx[k - 2], idx[k]]
                        }
                    })
                    .collect(),
                6 => (2..idx.len()).map(|k| [idx[0], idx[k - 1], idx[k]]).collect(),
                _ => {
                    skipped += 1;
                    continue;
                }
            };
            if det3(&m) < 0.0 {
                for t in &mut tris {
                    t.swap(1, 2);
                }
            }
            let mat = match prim["material"].as_u64() {
                Some(k) => j["materials"][k as usize]["name"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Material {k}")),
                None => "Default".into(),
            };
            let s = c.slot(&mat);
            c.add(s, &pos, tris.into_iter());
        }
    }
    if skipped > 0 {
        out.warnings.push(format!("{skipped} primitives without triangles skipped"));
    }
    out.parts = c.parts();
    Ok(out)
}
