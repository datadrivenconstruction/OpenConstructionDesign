//! glTF 2.0 reader (.gltf with embedded or external buffers, .glb) into a [`MeshSet`].
//!
//! Node transforms are composed, the scene is turned from Y-up to the model's Z-up
//! (`(x, y, z)` -> `(x, -z, y)`, the inverse of the export writer's root node), and every node
//! with a mesh becomes one part per primitive.

use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::job::{Progress, CANCELLED};
use crate::meshx::{Material, MeshSet, Part, GENERIC};

#[cfg(test)]
#[path = "read_tests.rs"]
mod read_tests;

const CHUNK_JSON: u32 = 0x4E4F_534A;
const CHUNK_BIN: u32 = 0x004E_4942;
const MAX_DEPTH: usize = 256;
const MAX_VISITS: usize = 5_000_000;
const MAX_WARNINGS: usize = 20;
/// Compression schemes that change how geometry is stored; this reader cannot decode them.
const UNDECODABLE: [&str; 2] = ["EXT_meshopt_compression", "KHR_draco_mesh_compression"];

enum E {
    /// The piece is not supported; the reader goes on without it.
    Skip(String),
    Fail(String),
    Cancelled,
}

impl From<String> for E {
    fn from(s: String) -> E {
        E::Fail(s)
    }
}

type R<T> = Result<T, E>;
type M = [f64; 16];

const IDENT: M = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// Reads a .glb or .gltf held in memory. `base_dir` resolves external buffer files; `name`
/// leads every error message.
pub fn read_bytes(
    bytes: &[u8],
    name: &str,
    base_dir: Option<&Path>,
    progress: Option<&Progress>,
) -> Result<(MeshSet, Vec<String>), String> {
    match run(bytes, base_dir, progress) {
        Ok(r) => Ok(r),
        Err(E::Cancelled) => Err(CANCELLED.into()),
        Err(E::Skip(s) | E::Fail(s)) => Err(format!("{name}: {s}")),
    }
}

/// Reads a .glb or .gltf file; external buffers are looked up next to it.
pub fn read_file(
    path: &Path,
    progress: Option<&Progress>,
) -> Result<(MeshSet, Vec<String>), String> {
    let name = path
        .file_name()
        .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.2);
        p.set_bytes(true);
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: cannot read the file: {e}", path.display()))?;
    read_bytes(&bytes, &name, path.parent(), progress)
}

fn run(
    bytes: &[u8],
    base_dir: Option<&Path>,
    progress: Option<&Progress>,
) -> R<(MeshSet, Vec<String>)> {
    let cancelled = || progress.is_some_and(Progress::is_cancelled);
    if let Some(p) = progress {
        p.phase("job.stage.read", 0.0, 0.2);
    }
    if cancelled() {
        return Err(E::Cancelled);
    }
    let (j, bin) = split(bytes)?;
    let version = j["asset"]["version"].as_str().unwrap_or("");
    if !version.starts_with('2') {
        return Err(E::Fail(format!(
            "not a glTF 2.0 file (asset.version is {})",
            if version.is_empty() { "missing" } else { version }
        )));
    }
    let mut warns = vec![];
    let mut quant = false;
    for e in j["extensionsRequired"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        if UNDECODABLE.contains(&e) {
            return Err(E::Fail(format!(
                "the file requires the extension {e}, which this reader cannot decode; \
                 export it without mesh compression"
            )));
        }
        match e {
            "KHR_mesh_quantization" => quant = true,
            _ if e.starts_with("KHR_materials_") || e.starts_with("KHR_texture_") => {}
            _ => warns.push(format!("required extension {e} is not supported and was ignored")),
        }
    }
    quant |= j["extensionsUsed"]
        .as_array()
        .is_some_and(|a| a.iter().any(|e| e == "KHR_mesh_quantization"));
    let bufs = load_buffers(&j, bin, base_dir, &cancelled)?;
    if let Some(p) = progress {
        p.phase("job.stage.parse", 0.2, 0.8);
    }
    let doc = Doc { j: &j, bufs, quant };
    let set = build(&doc, progress, &mut warns)?;
    if set.parts.is_empty() {
        return Err(E::Fail(format!(
            "no triangle geometry in the scene{}",
            if warns.is_empty() {
                String::new()
            } else {
                format!(" ({})", warns.join("; "))
            }
        )));
    }
    if warns.len() > MAX_WARNINGS {
        let more = warns.len() - MAX_WARNINGS;
        warns.truncate(MAX_WARNINGS);
        warns.push(format!("... and {more} more warning(s)"));
    }
    Ok((set, warns))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4)
        .and_then(|s| s.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

/// JSON document and the GLB BIN chunk, if any.
fn split(bytes: &[u8]) -> R<(Value, Option<&[u8]>)> {
    let json_err = |e: serde_json::Error| E::Fail(format!("invalid glTF JSON: {e}"));
    if !bytes.starts_with(b"glTF") {
        let text = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
        return Ok((serde_json::from_slice(text).map_err(json_err)?, None));
    }
    if bytes.len() < 20 {
        return Err(E::Fail(format!("truncated GLB: {} bytes, the header needs 20", bytes.len())));
    }
    let version = u32_at(bytes, 4);
    if version != 2 {
        return Err(E::Fail(format!("GLB version {version} is not supported (need 2)")));
    }
    let declared = u32_at(bytes, 8) as usize;
    if declared > bytes.len() {
        return Err(E::Fail(format!(
            "truncated GLB: the header declares {declared} bytes, the file has {}",
            bytes.len()
        )));
    }
    let body = &bytes[..declared.max(12)];
    let (mut js, mut bin) = (None, None);
    let mut at = 12usize;
    while at < body.len() {
        if body.len() - at < 8 {
            return Err(E::Fail(format!("truncated GLB: chunk header at byte {at} is cut off")));
        }
        let len = u32_at(body, at) as usize;
        let kind = u32_at(body, at + 4);
        let start = at + 8;
        let end = start.checked_add(len).filter(|&e| e <= body.len()).ok_or_else(|| {
            E::Fail(format!(
                "truncated GLB chunk at byte {at}: it declares {len} bytes, {} remain",
                body.len() - start
            ))
        })?;
        match kind {
            CHUNK_JSON if js.is_none() => js = Some(&body[start..end]),
            CHUNK_BIN if bin.is_none() => bin = Some(&body[start..end]),
            _ => {}
        }
        at = end;
    }
    let mut js = js.ok_or_else(|| E::Fail("GLB has no JSON chunk".into()))?;
    while let [rest @ .., 0 | b' '] = js {
        js = rest;
    }
    Ok((serde_json::from_slice(js).map_err(json_err)?, bin))
}

fn percent_decode(s: &str) -> Result<String, String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            out.push(hex.ok_or_else(|| format!("bad %-escape in uri '{s}'"))?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("uri '{s}' is not valid UTF-8"))
}

fn external_path(uri: &str, base: &Path, i: usize) -> Result<PathBuf, String> {
    let rel = percent_decode(uri)?;
    let bad = rel.is_empty()
        || rel.starts_with(['/', '\\'])
        || rel.contains(':')
        || rel.split(['/', '\\']).any(|c| c == "..");
    if bad {
        return Err(format!(
            "buffer {i}: uri '{uri}' must be a relative path inside the file's folder \
             (no absolute path, drive, URL or '..')"
        ));
    }
    Ok(base.join(rel))
}

fn load_buffers<'a>(
    j: &Value,
    glb_bin: Option<&'a [u8]>,
    base_dir: Option<&Path>,
    cancelled: &dyn Fn() -> bool,
) -> R<Vec<Cow<'a, [u8]>>> {
    let mut out = vec![];
    for (i, b) in j["buffers"].as_array().into_iter().flatten().enumerate() {
        if cancelled() {
            return Err(E::Cancelled);
        }
        let declared = uget(b, "byteLength")
            .ok_or_else(|| format!("buffer {i} has no valid byteLength"))?;
        let data: Cow<[u8]> = match b["uri"].as_str() {
            None => match (i, glb_bin) {
                (0, Some(bin)) => Cow::Borrowed(bin),
                _ => return Err(E::Fail(format!("buffer {i} has no uri and the file has no BIN chunk for it"))),
            },
            Some(uri) if uri.starts_with("data:") => {
                let (head, body) = uri
                    .split_once(',')
                    .ok_or_else(|| format!("buffer {i}: data uri has no payload"))?;
                if !head.ends_with(";base64") {
                    return Err(E::Fail(format!("buffer {i}: only base64 data uris are supported")));
                }
                let mut s: String = body.chars().filter(|c| !c.is_whitespace()).collect();
                while s.len() % 4 != 0 {
                    s.push('=');
                }
                Cow::Owned(
                    crate::bridge::sig::b64(&s)
                        .map_err(|e| format!("buffer {i}: data uri is not valid base64 ({e})"))?,
                )
            }
            Some(uri) => {
                let base = base_dir.ok_or_else(|| {
                    format!("buffer {i} is the external file '{uri}' but the file's folder is unknown")
                })?;
                let path = external_path(uri, base, i)?;
                Cow::Owned(
                    std::fs::read(&path)
                        .map_err(|e| format!("buffer {i}: cannot read {}: {e}", path.display()))?,
                )
            }
        };
        if data.len() < declared {
            return Err(E::Fail(format!(
                "buffer {i} is truncated: byteLength {declared}, only {} bytes available",
                data.len()
            )));
        }
        out.push(data);
    }
    Ok(out)
}

fn uget(v: &Value, k: &str) -> Option<usize> {
    v.get(k)?.as_u64().and_then(|x| usize::try_from(x).ok())
}

fn csize(ctype: usize) -> Option<usize> {
    Some(match ctype {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        _ => return None,
    })
}

struct Acc<'a> {
    data: &'a [u8],
    off: usize,
    stride: usize,
    count: usize,
    ctype: usize,
    norm: bool,
}

impl Acc<'_> {
    fn bytes<const N: usize>(&self, at: usize) -> [u8; N] {
        self.data
            .get(at..at + N)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; N])
    }

    /// Component `c` of element `k`; ranges were checked when the accessor was opened.
    fn get(&self, k: usize, c: usize) -> f64 {
        let at = self.off + k * self.stride + c * csize(self.ctype).unwrap_or(0);
        let n = |v: f64, max: f64| if self.norm { (v / max).max(-1.0) } else { v };
        match self.ctype {
            5126 => f64::from(f32::from_le_bytes(self.bytes(at))),
            5125 => f64::from(u32::from_le_bytes(self.bytes(at))),
            5123 => n(f64::from(u16::from_le_bytes(self.bytes(at))), 65535.0),
            5122 => n(f64::from(i16::from_le_bytes(self.bytes(at))), 32767.0),
            5121 => n(f64::from(self.bytes::<1>(at)[0]), 255.0),
            _ => n(f64::from(i8::from_le_bytes(self.bytes(at))), 127.0),
        }
    }
}

struct Doc<'a> {
    j: &'a Value,
    bufs: Vec<Cow<'a, [u8]>>,
    quant: bool,
}

impl Doc<'_> {
    fn accessor(&self, i: usize, want: &str) -> R<Acc<'_>> {
        let fail = |s: String| E::Fail(format!("accessor {i}: {s}"));
        let a = self.j["accessors"]
            .get(i)
            .ok_or_else(|| E::Fail(format!("accessor {i} does not exist")))?;
        if a.get("sparse").is_some() {
            return Err(E::Skip(format!("accessor {i} is sparse, which is not supported")));
        }
        let ty = a["type"].as_str().unwrap_or("");
        if ty != want {
            return Err(fail(format!("type is '{ty}', expected {want}")));
        }
        let ncomp = if want == "VEC3" { 3 } else { 1 };
        let ctype = uget(a, "componentType").unwrap_or(0);
        let cs = csize(ctype).ok_or_else(|| fail(format!("unknown componentType {ctype}")))?;
        let count = uget(a, "count").ok_or_else(|| fail("no valid count".into()))?;
        let Some(bv) = uget(a, "bufferView") else {
            return Err(E::Skip(format!(
                "accessor {i} has no bufferView (compressed or zero-filled data)"
            )));
        };
        let view = self.j["bufferViews"]
            .get(bv)
            .ok_or_else(|| fail(format!("bufferView {bv} does not exist")))?;
        let bi = uget(view, "buffer").unwrap_or(usize::MAX);
        let data: &[u8] = self
            .bufs
            .get(bi)
            .ok_or_else(|| fail(format!("bufferView {bv} uses buffer {bi}, which does not exist")))?;
        let voff = uget(view, "byteOffset").unwrap_or(0);
        let vlen = uget(view, "byteLength")
            .ok_or_else(|| fail(format!("bufferView {bv} has no byteLength")))?;
        if voff.checked_add(vlen).is_none_or(|e| e > data.len()) {
            return Err(fail(format!(
                "bufferView {bv} (offset {voff}, length {vlen}) is past the end of buffer {bi} ({} bytes)",
                data.len()
            )));
        }
        let elem = ncomp * cs;
        let stride = uget(view, "byteStride").unwrap_or(elem);
        if stride < elem {
            return Err(fail(format!("bufferView {bv} byteStride {stride} is below the element size {elem}")));
        }
        let aoff = uget(a, "byteOffset").unwrap_or(0);
        if count > 0 {
            let end = (count - 1)
                .checked_mul(stride)
                .and_then(|x| x.checked_add(aoff))
                .and_then(|x| x.checked_add(elem));
            if end.is_none_or(|e| e > vlen) {
                return Err(fail(format!(
                    "{count} elements of {elem} bytes (stride {stride}, offset {aoff}) do not fit \
                     bufferView {bv} of {vlen} bytes"
                )));
            }
        }
        Ok(Acc {
            data,
            off: voff + aoff,
            stride,
            count,
            ctype,
            norm: a["normalized"].as_bool().unwrap_or(false),
        })
    }

    fn positions(&self, i: usize) -> R<Vec<[f64; 3]>> {
        let a = self.accessor(i, "VEC3")?;
        if a.ctype != 5126 && !self.quant {
            return Err(E::Skip(format!(
                "POSITION accessor {i} is not FLOAT (integer positions need KHR_mesh_quantization)"
            )));
        }
        Ok((0..a.count).map(|k| [a.get(k, 0), a.get(k, 1), a.get(k, 2)]).collect())
    }

    fn indices(&self, i: usize, npos: usize) -> R<Vec<u32>> {
        let a = self.accessor(i, "SCALAR")?;
        if !matches!(a.ctype, 5121 | 5123 | 5125) {
            return Err(E::Fail(format!(
                "accessor {i}: indices must be UNSIGNED_BYTE, UNSIGNED_SHORT or UNSIGNED_INT"
            )));
        }
        let mut out = Vec::with_capacity(a.count);
        for k in 0..a.count {
            let v = a.get(k, 0) as u32;
            if v as usize >= npos {
                return Err(E::Fail(format!(
                    "accessor {i}: index {v} at element {k} is out of range \
                     (the primitive has {npos} vertices)"
                )));
            }
            out.push(v);
        }
        Ok(out)
    }
}

fn mul(a: &M, b: &M) -> M {
    let mut o = [0.0; 16];
    for c in 0..4 {
        for r in 0..4 {
            o[c * 4 + r] = (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum();
        }
    }
    o
}

fn det3(m: &M) -> f64 {
    m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2])
}

fn floats<const N: usize>(v: Option<&Value>, what: &str, default: [f64; N]) -> Result<[f64; N], String> {
    let Some(v) = v else {
        return Ok(default);
    };
    let bad = || format!("{what} must be {N} finite numbers");
    let a = v.as_array().filter(|a| a.len() == N).ok_or_else(bad)?;
    let mut out = [0.0; N];
    for (o, x) in out.iter_mut().zip(a) {
        *o = x.as_f64().filter(|f| f.is_finite()).ok_or_else(bad)?;
    }
    Ok(out)
}

fn local_matrix(i: usize, n: &Value) -> Result<M, String> {
    if n.get("matrix").is_some() {
        return floats::<16>(n.get("matrix"), &format!("node {i}: matrix"), IDENT);
    }
    let t = floats::<3>(n.get("translation"), &format!("node {i}: translation"), [0.0; 3])?;
    let q = floats::<4>(n.get("rotation"), &format!("node {i}: rotation"), [0.0, 0.0, 0.0, 1.0])?;
    let s = floats::<3>(n.get("scale"), &format!("node {i}: scale"), [1.0; 3])?;
    let len = q.iter().map(|v| v * v).sum::<f64>().sqrt();
    let [x, y, z, w] = if len < 1e-12 { [0.0, 0.0, 0.0, 1.0] } else { q.map(|v| v / len) };
    let r = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    let mut m = IDENT;
    for c in 0..3 {
        for row in 0..3 {
            m[c * 4 + row] = r[row][c] * s[c];
        }
        m[12 + c] = t[c];
    }
    Ok(m)
}

/// World point in model axes (Z up).
fn place(m: &M, p: [f64; 3]) -> [f64; 3] {
    let w = [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r]);
    [w[0], -w[2], w[1]]
}

fn triangles(mode: u64, idx: &[u32]) -> Vec<[u32; 3]> {
    let mut t = vec![];
    match mode {
        4 => t.extend(idx.chunks_exact(3).map(|c| [c[0], c[1], c[2]])),
        5 => {
            for k in 0..idx.len().saturating_sub(2) {
                let (a, b, c) = (idx[k], idx[k + 1], idx[k + 2]);
                if a == b || b == c || a == c {
                    continue;
                }
                t.push(if k % 2 == 0 { [a, b, c] } else { [b, a, c] });
            }
        }
        _ => {
            for k in 1..idx.len().saturating_sub(1) {
                t.push([idx[0], idx[k], idx[k + 1]]);
            }
        }
    }
    t
}

fn factor4(v: Option<&Value>) -> Option<[f32; 4]> {
    floats::<4>(v, "", [0.0; 4]).ok().filter(|_| v.is_some()).map(|a| a.map(|c| c as f32))
}

fn material_of(j: &Value, i: usize) -> Result<Material, String> {
    let m = j["materials"].get(i).ok_or_else(|| format!("material {i} does not exist"))?;
    let name = m["name"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(|| format!("material {i}"), str::to_string);
    let mut rgba = factor4(m["pbrMetallicRoughness"].get("baseColorFactor"))
        .or_else(|| {
            factor4(m["extensions"]["KHR_materials_pbrSpecularGlossiness"].get("diffuseFactor"))
        })
        .unwrap_or([1.0; 4]);
    if m["alphaMode"].as_str().unwrap_or("OPAQUE") == "OPAQUE" {
        rgba[3] = 1.0;
    }
    Ok(Material { name, rgba })
}

struct Prim {
    positions: Vec<[f64; 3]>,
    triangles: Vec<[u32; 3]>,
}

fn primitive(doc: &Doc, p: &Value, world: &M, nonsurface: &mut usize) -> R<Option<Prim>> {
    let mode = p["mode"].as_u64().unwrap_or(4);
    if mode <= 3 {
        *nonsurface += 1;
        return Ok(None);
    }
    if mode > 6 {
        return Err(E::Skip(format!("unknown primitive mode {mode}")));
    }
    let pa = uget(&p["attributes"], "POSITION")
        .ok_or_else(|| E::Skip("no POSITION attribute".into()))?;
    let positions: Vec<[f64; 3]> = doc.positions(pa)?.into_iter().map(|v| place(world, v)).collect();
    if u32::try_from(positions.len()).is_err() {
        return Err(E::Fail(format!("accessor {pa}: more than 2^32 vertices")));
    }
    let idx = match uget(p, "indices") {
        Some(ia) => doc.indices(ia, positions.len())?,
        None => (0..positions.len() as u32).collect(),
    };
    let mut tris = triangles(mode, &idx);
    if det3(world) < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    Ok(Some(Prim { positions, triangles: tris }))
}

fn build(doc: &Doc, progress: Option<&Progress>, warns: &mut Vec<String>) -> R<MeshSet> {
    let j = doc.j;
    let nodes = j["nodes"].as_array().map_or(&[][..], Vec::as_slice);
    let node_ids = |v: &Value, what: &str| -> Result<Vec<usize>, String> {
        v.as_array()
            .into_iter()
            .flatten()
            .map(|x| {
                x.as_u64()
                    .and_then(|x| usize::try_from(x).ok())
                    .filter(|&x| x < nodes.len())
                    .ok_or_else(|| format!("{what} lists node {x}, which does not exist"))
            })
            .collect()
    };
    let scenes = j["scenes"].as_array().map_or(&[][..], Vec::as_slice);
    let roots = if let Some(s) = j.get("scene").and_then(Value::as_u64) {
        let sc = scenes
            .get(s as usize)
            .ok_or_else(|| format!("scene {s} does not exist"))?;
        node_ids(&sc["nodes"], &format!("scene {s}"))?
    } else if let Some(sc) = scenes.first() {
        node_ids(&sc["nodes"], "scene 0")?
    } else {
        let mut child = vec![false; nodes.len()];
        for n in nodes {
            for c in node_ids(&n["children"], "a node")? {
                child[c] = true;
            }
        }
        (0..nodes.len()).filter(|&i| !child[i]).collect()
    };

    if let Some(p) = progress {
        p.set_total(nodes.len() as u64);
        p.set(0);
    }
    let mut out = MeshSet::default();
    let mut mat_of: Vec<Option<usize>> = vec![None; j["materials"].as_array().map_or(0, Vec::len)];
    let mut keys: HashSet<String> = HashSet::new();
    let (mut nonsurface, mut empty, mut instancing) = (0usize, 0usize, false);
    let mut stack: Vec<(usize, M, usize)> = roots.iter().rev().map(|&r| (r, IDENT, 0)).collect();
    let mut visits = 0usize;
    while let Some((i, parent, depth)) = stack.pop() {
        visits += 1;
        if visits > MAX_VISITS || depth > MAX_DEPTH {
            return Err(E::Fail(
                "the node graph is cyclic or too deep (over 256 levels or 5 million visits)".into(),
            ));
        }
        if let Some(p) = progress {
            if p.is_cancelled() {
                return Err(E::Cancelled);
            }
            p.set(visits as u64);
        }
        let n = &nodes[i];
        let world = mul(&parent, &local_matrix(i, n)?);
        instancing |= n["extensions"].get("EXT_mesh_gpu_instancing").is_some();
        if let Some(mi) = uget(n, "mesh") {
            let mesh = j["meshes"]
                .get(mi)
                .ok_or_else(|| format!("node {i}: mesh {mi} does not exist"))?;
            let d = &n["extras"]["ddc"];
            let base_key = match (d["table"].as_str(), d["id"].as_i64()) {
                (Some(t), Some(id)) => format!("{t}:{id}"),
                _ => format!("gltf:{i}"),
            };
            let name = n["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| mesh["name"].as_str().filter(|s| !s.trim().is_empty()))
                .map_or_else(|| format!("node {i}"), str::to_string);
            for (pi, p) in mesh["primitives"].as_array().into_iter().flatten().enumerate() {
                if progress.is_some_and(Progress::is_cancelled) {
                    return Err(E::Cancelled);
                }
                let ctx = |e: E| match e {
                    E::Fail(s) => E::Fail(format!("mesh {mi} primitive {pi}: {s}")),
                    E::Skip(s) => E::Skip(format!("mesh {mi} primitive {pi}: {s}")),
                    E::Cancelled => E::Cancelled,
                };
                let prim = match primitive(doc, p, &world, &mut nonsurface) {
                    Ok(Some(x)) => x,
                    Ok(None) => continue,
                    Err(E::Skip(s)) => {
                        warns.push(format!("mesh {mi} primitive {pi} skipped: {s}"));
                        continue;
                    }
                    Err(e) => return Err(ctx(e)),
                };
                let material = match uget(p, "material") {
                    None => None,
                    Some(m) => {
                        let slot = mat_of
                            .get_mut(m)
                            .ok_or_else(|| format!("mesh {mi} primitive {pi}: material {m} does not exist"))?;
                        if slot.is_none() {
                            out.materials.push(material_of(j, m)?);
                            *slot = Some(out.materials.len() - 1);
                        }
                        *slot
                    }
                };
                let mut key = if pi == 0 { base_key.clone() } else { format!("{base_key}#{pi}") };
                let mut dup = 1;
                while !keys.insert(key.clone()) {
                    dup += 1;
                    key = format!("{base_key}#{pi}~{dup}");
                }
                let part = Part {
                    key,
                    name: name.clone(),
                    category: d["category"].as_str().unwrap_or(GENERIC).to_string(),
                    level: d["level"].as_str().map(str::to_string),
                    material,
                    positions: prim.positions,
                    triangles: prim.triangles,
                };
                if !out.push(part) {
                    empty += 1;
                }
            }
        }
        let kids = node_ids(&n["children"], &format!("node {i}"))?;
        stack.extend(kids.iter().rev().map(|&c| (c, world, depth + 1)));
    }
    if nonsurface > 0 {
        warns.push(format!("{nonsurface} point or line primitive(s) skipped"));
    }
    if empty > 0 {
        warns.push(format!("{empty} primitive(s) had no valid triangles"));
    }
    if instancing {
        warns.push("EXT_mesh_gpu_instancing is not supported; instanced meshes are placed once".into());
    }
    Ok(out)
}
