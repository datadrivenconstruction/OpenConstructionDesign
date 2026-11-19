//! glTF 2.0 binary (.glb) export. Own writer, no dependencies beyond `serde_json`.
//!
//! The model is Z-up in metres; glTF is Y-up. One root node carries the -90° turn about X, so
//! vertex data stays in model axes and element extras keep model coordinates.
//! Parts that repeat (same shape up to a translation or a full matrix) share one mesh: the node
//! list grows, the buffer does not. Materials are per category; element ids and properties
//! travel in node `extras`.

use serde_json::Map;
/// Extras are plain JSON; callers without their own `serde_json` use these.
pub use serde_json::{json, Value};

/// JSON object for extras.
pub type Props = Map<String, Value>;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;

mod read;
pub use read::{read_bytes, read_file};

#[cfg(test)]
mod tests;

/// Shapes closer than this (m) count as equal when looking for repeats.
const REPEAT_TOL: f32 = 1e-4;

#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    /// Linear RGBA; alpha below 1 becomes `BLEND`.
    pub color: [f32; 4],
    pub extras: Value,
}

#[derive(Clone, Debug)]
pub struct Mesh {
    /// Local coordinates (model axes, Z up).
    pub positions: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
    pub material: usize,
}

#[derive(Clone, Debug)]
pub struct Node {
    pub name: String,
    pub mesh: usize,
    /// Column-major 4x4; `None` — translation only.
    pub matrix: Option<[f32; 16]>,
    pub translation: [f32; 3],
    pub extras: Value,
}

#[derive(Default, Debug)]
pub struct Scene {
    pub name: String,
    pub materials: Vec<Material>,
    pub meshes: Vec<Mesh>,
    pub nodes: Vec<Node>,
    shapes: HashMap<u64, Vec<usize>>,
}

impl Scene {
    pub fn new(name: &str) -> Self {
        Scene {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Material by name (a repeated name returns the first one).
    pub fn material(&mut self, name: &str, color: [f32; 4], extras: Value) -> usize {
        if let Some(i) = self.materials.iter().position(|m| m.name == name) {
            return i;
        }
        self.materials.push(Material {
            name: name.into(),
            color,
            extras,
        });
        self.materials.len() - 1
    }

    /// A part at its model position. The mesh is stored relative to its bounding-box minimum;
    /// an equal shape already stored (same material, within `REPEAT_TOL`) is reused and only a
    /// node is added. Returns the node index, `None` for an empty or invalid part.
    pub fn add_part(
        &mut self,
        name: &str,
        material: usize,
        positions: &[[f32; 3]],
        triangles: &[[u32; 3]],
        extras: Value,
    ) -> Option<usize> {
        if material >= self.materials.len() || !valid_mesh(positions, triangles) {
            return None;
        }
        let mut origin = [f32::INFINITY; 3];
        for p in positions {
            for k in 0..3 {
                origin[k] = origin[k].min(p[k]);
            }
        }
        let local: Vec<[f32; 3]> = positions
            .iter()
            .map(|p| [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]])
            .collect();
        let key = shape_key(material, &local, triangles);
        let found = self.shapes.get(&key).and_then(|c| {
            c.iter()
                .copied()
                .find(|&i| same_shape(&self.meshes[i], material, &local, triangles))
        });
        let mesh = match found {
            Some(i) => i,
            None => {
                self.meshes.push(Mesh {
                    positions: local,
                    triangles: triangles.to_vec(),
                    material,
                });
                let i = self.meshes.len() - 1;
                self.shapes.entry(key).or_default().push(i);
                i
            }
        };
        self.nodes.push(Node {
            name: name.into(),
            mesh,
            matrix: None,
            translation: origin,
            extras,
        });
        Some(self.nodes.len() - 1)
    }

    /// One mesh placed many times (`placements`: column-major matrix and node extras).
    /// Returns the number of nodes added.
    pub fn add_instances(
        &mut self,
        name: &str,
        material: usize,
        positions: &[[f32; 3]],
        triangles: &[[u32; 3]],
        placements: &[([f32; 16], Value)],
    ) -> usize {
        if material >= self.materials.len() || !valid_mesh(positions, triangles) {
            return 0;
        }
        self.meshes.push(Mesh {
            positions: positions.to_vec(),
            triangles: triangles.to_vec(),
            material,
        });
        let mesh = self.meshes.len() - 1;
        for (m, extras) in placements {
            self.nodes.push(Node {
                name: name.into(),
                mesh,
                matrix: Some(*m),
                translation: [0.0; 3],
                extras: extras.clone(),
            });
        }
        placements.len()
    }

    /// Triangles over all nodes (shared meshes count once per node).
    pub fn triangle_count(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| self.meshes[n.mesh].triangles.len())
            .sum()
    }
}

fn valid_mesh(p: &[[f32; 3]], t: &[[u32; 3]]) -> bool {
    !p.is_empty()
        && !t.is_empty()
        && p.iter().all(|v| v.iter().all(|c| c.is_finite()))
        && t.iter().all(|f| f.iter().all(|&i| (i as usize) < p.len()))
}

fn q(v: f32) -> i64 {
    (v / REPEAT_TOL).round() as i64
}

fn shape_key(material: usize, local: &[[f32; 3]], tris: &[[u32; 3]]) -> u64 {
    let mut h = DefaultHasher::new();
    material.hash(&mut h);
    local.len().hash(&mut h);
    tris.hash(&mut h);
    for p in local {
        for c in p {
            q(*c).hash(&mut h);
        }
    }
    h.finish()
}

fn same_shape(m: &Mesh, material: usize, local: &[[f32; 3]], tris: &[[u32; 3]]) -> bool {
    m.material == material
        && m.triangles == tris
        && m.positions.len() == local.len()
        && m.positions
            .iter()
            .zip(local)
            .all(|(a, b)| (0..3).all(|k| q(a[k]) == q(b[k])))
}

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn pad4(v: &mut Vec<u8>, byte: u8) {
    while v.len() % 4 != 0 {
        v.push(byte);
    }
}

fn num(v: f32) -> Value {
    json!(f64::from(v))
}

/// The scene as a `.glb` byte vector.
pub fn write_glb(scene: &Scene) -> Result<Vec<u8>, String> {
    for (i, m) in scene.meshes.iter().enumerate() {
        if m.material >= scene.materials.len() || !valid_mesh(&m.positions, &m.triangles) {
            return Err(format!("gltf: mesh {i} is invalid"));
        }
    }
    if let Some(n) = scene.nodes.iter().find(|n| {
        n.mesh >= scene.meshes.len()
            || n.translation.iter().any(|v| !v.is_finite())
            || n.matrix.is_some_and(|m| m.iter().any(|v| !v.is_finite()))
    }) {
        return Err(format!("gltf: node '{}' is invalid", n.name));
    }

    let mut bin: Vec<u8> = vec![];
    let (mut views, mut accessors, mut meshes) = (vec![], vec![], vec![]);
    for m in &scene.meshes {
        let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        let start = bin.len();
        for p in &m.positions {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
                push_f32(&mut bin, p[k]);
            }
        }
        views.push(
            json!({"buffer": 0, "byteOffset": start, "byteLength": bin.len() - start,
            "target": 34962}),
        );
        accessors.push(json!({"bufferView": views.len() - 1, "componentType": 5126,
            "count": m.positions.len(), "type": "VEC3",
            "min": lo.map(num), "max": hi.map(num)}));
        let pos_acc = accessors.len() - 1;
        let start = bin.len();
        for t in &m.triangles {
            for i in t {
                bin.extend_from_slice(&i.to_le_bytes());
            }
        }
        views.push(
            json!({"buffer": 0, "byteOffset": start, "byteLength": bin.len() - start,
            "target": 34963}),
        );
        accessors.push(json!({"bufferView": views.len() - 1, "componentType": 5125,
            "count": m.triangles.len() * 3, "type": "SCALAR"}));
        meshes.push(json!({"primitives": [{"attributes": {"POSITION": pos_acc},
            "indices": accessors.len() - 1, "material": m.material, "mode": 4}]}));
    }

    let materials: Vec<Value> = scene
        .materials
        .iter()
        .map(|m| {
            let mode = if m.color[3] < 1.0 { "BLEND" } else { "OPAQUE" };
            let mut o = json!({"name": m.name,
                "pbrMetallicRoughness": {"baseColorFactor": m.color.map(num),
                    "metallicFactor": 0.0, "roughnessFactor": 0.85},
                "alphaMode": mode,
                "doubleSided": true});
            if !m.extras.is_null() {
                o["extras"] = m.extras.clone();
            }
            o
        })
        .collect();

    // node 0 turns Z-up into Y-up; elements hang below it
    let mut top = json!({"name": "Z-up",
        "rotation": [-std::f64::consts::FRAC_1_SQRT_2, 0.0, 0.0, std::f64::consts::FRAC_1_SQRT_2]});
    if !scene.nodes.is_empty() {
        top["children"] = json!((1..=scene.nodes.len()).collect::<Vec<_>>());
    }
    let mut nodes = vec![top];
    for n in &scene.nodes {
        let mut o = Map::new();
        o.insert("name".into(), json!(n.name));
        o.insert("mesh".into(), json!(n.mesh));
        match n.matrix {
            Some(m) => {
                o.insert("matrix".into(), json!(m.map(num)));
            }
            None if n.translation != [0.0; 3] => {
                o.insert("translation".into(), json!(n.translation.map(num)));
            }
            None => {}
        }
        if !n.extras.is_null() {
            o.insert("extras".into(), n.extras.clone());
        }
        nodes.push(Value::Object(o));
    }

    let mut root = json!({
        "asset": {"version": "2.0", "generator": "OpenConstructionDesign"},
        "scene": 0,
        "scenes": [{"name": scene.name, "nodes": [0], "extras": {"units": "m", "up": "Z"}}],
        "nodes": nodes,
    });
    if !materials.is_empty() {
        root["materials"] = Value::Array(materials);
    }
    if !meshes.is_empty() {
        root["meshes"] = Value::Array(meshes);
        root["accessors"] = Value::Array(accessors);
        root["bufferViews"] = Value::Array(views);
        root["buffers"] = json!([{"byteLength": bin.len()}]);
    }
    let mut js = serde_json::to_vec(&root).map_err(|e| format!("gltf: {e}"))?;
    pad4(&mut js, b' ');
    pad4(&mut bin, 0);

    let total = 12 + 8 + js.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let total32 = u32::try_from(total).map_err(|_| "gltf: scene is over 4 GiB".to_string())?;
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&total32.to_le_bytes());
    out.extend_from_slice(&(js.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
    out.extend_from_slice(&js);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        out.extend_from_slice(&bin);
    }
    Ok(out)
}

/// The scene as one `.gltf` JSON text, its buffer embedded as a base64 data URI.
pub fn write_gltf(scene: &Scene) -> Result<String, String> {
    let glb = write_glb(scene)?;
    let chunk = |at: usize| -> Option<(u32, &[u8])> {
        let len = u32::from_le_bytes(glb.get(at..at + 4)?.try_into().ok()?) as usize;
        let kind = u32::from_le_bytes(glb.get(at + 4..at + 8)?.try_into().ok()?);
        Some((kind, glb.get(at + 8..at + 8 + len)?))
    };
    let (_, js) = chunk(12).ok_or("gltf: no JSON chunk")?;
    let mut root: Value = serde_json::from_slice(js).map_err(|e| format!("gltf: {e}"))?;
    if let Some((_, bin)) = chunk(20 + js.len()) {
        // the chunk is padded to 4 bytes; the buffer is exactly its stated length
        let n = root["buffers"][0]["byteLength"].as_u64().unwrap_or(bin.len() as u64) as usize;
        let bin = bin.get(..n).ok_or("gltf: buffer is shorter than stated")?;
        let uri = format!(
            "data:application/octet-stream;base64,{}",
            crate::bridge::sig::b64_encode(bin)
        );
        root["buffers"][0]["uri"] = Value::String(uri);
    }
    serde_json::to_string(&root).map_err(|e| format!("gltf: {e}"))
}

/// Writes `scene` to `path` (.glb).
pub fn write_glb_file(scene: &Scene, path: &Path) -> Result<(), String> {
    let bytes = write_glb(scene)?;
    std::fs::write(path, bytes).map_err(|e| format!("gltf: {e}"))
}
