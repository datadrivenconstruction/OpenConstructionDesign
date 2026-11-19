//! `geometry.glb` of a revision (contract README s3): Y-up metres, one node per element named by
//! its key, extras `{element_key, type_key, ifc_class}` only, equal shapes share one mesh,
//! triangles with normals, colour by `ifc_class`. The root-less layout keeps vertex data in the
//! frame the Parquet `bbox` and `geometry_hash` use.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// One element's triangles in GLB space.
#[derive(Clone, Debug, Default)]
pub struct Body {
    pub positions: Vec<[f32; 3]>,
    pub tris: Vec<[u32; 3]>,
}

/// Model (Z-up) point to GLB (Y-up).
pub fn to_glb(p: [f32; 3]) -> [f32; 3] {
    [p[0], p[2], -p[1]]
}

impl Body {
    /// Adds a mesh placed by `place` (model space); a mirroring placement flips the winding.
    pub fn add(&mut self, pos: &[[f32; 3]], tris: &[[u32; 3]], place: &dyn Fn([f32; 3]) -> [f32; 3], flip: bool) {
        let base = self.positions.len() as u32;
        self.positions.extend(pos.iter().map(|p| to_glb(place(*p))));
        self.tris.extend(tris.iter().map(|t| {
            if flip {
                [t[0] + base, t[2] + base, t[1] + base]
            } else {
                [t[0] + base, t[1] + base, t[2] + base]
            }
        }));
    }

    /// Triangles that point at real, finite vertices; `false` when nothing usable is left.
    pub fn usable(&self) -> bool {
        !self.tris.is_empty()
            && self.positions.iter().all(|p| p.iter().all(|c| c.is_finite()))
            && self.tris.iter().all(|t| t.iter().all(|&i| (i as usize) < self.positions.len()))
    }

    /// min x, y, z, max x, y, z.
    pub fn bbox(&self) -> [f64; 6] {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for p in &self.positions {
            for k in 0..3 {
                b[k] = b[k].min(p[k] as f64);
                b[k + 3] = b[k + 3].max(p[k] as f64);
            }
        }
        b
    }
}

/// SHA-256 over the canonical triangle list (README s4): millimetre integers, each triangle
/// rotated to its smallest start, triangles sorted, little-endian int32.
pub fn geometry_hash(pos: &[[f32; 3]], tris: &[[u32; 3]]) -> String {
    let mm = |c: f32| (c as f64 * 1000.0).round() as i32;
    let mut all: Vec<[i32; 9]> = tris
        .iter()
        .map(|t| {
            let v: Vec<[i32; 3]> = t.iter().map(|&i| pos[i as usize].map(mm)).collect();
            (0..3)
                .map(|s| {
                    let mut a = [0i32; 9];
                    for k in 0..3 {
                        a[k * 3..k * 3 + 3].copy_from_slice(&v[(s + k) % 3]);
                    }
                    a
                })
                .min()
                .unwrap_or([0; 9])
        })
        .collect();
    all.sort_unstable();
    let mut h = Sha256::new();
    for t in &all {
        for c in t {
            h.update(c.to_le_bytes());
        }
    }
    hex(&h.finalize())
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Linear RGBA per `ifc_class`.
fn colour(class: &str) -> [f32; 4] {
    let c = match class {
        "IfcWall" | "IfcWallStandardCase" => [0.78, 0.76, 0.72],
        "IfcSlab" => [0.62, 0.62, 0.64],
        "IfcRoof" => [0.55, 0.30, 0.25],
        "IfcCovering" => [0.85, 0.85, 0.80],
        "IfcWindow" | "IfcCurtainWall" => [0.45, 0.65, 0.80],
        "IfcDoor" => [0.55, 0.38, 0.22],
        "IfcColumn" | "IfcBeam" | "IfcMember" | "IfcPile" => [0.55, 0.58, 0.62],
        "IfcStair" | "IfcStairFlight" | "IfcRamp" => [0.70, 0.68, 0.60],
        "IfcRailing" => [0.35, 0.35, 0.38],
        "IfcFooting" => [0.50, 0.50, 0.48],
        _ => [0.70, 0.70, 0.70],
    };
    [c[0], c[1], c[2], if class == "IfcWindow" || class == "IfcCurtainWall" { 0.6 } else { 1.0 }]
}

/// One node of the output.
pub struct Item {
    pub key: String,
    pub type_key: String,
    pub ifc_class: String,
    pub body: Body,
}

pub struct Built {
    pub glb: Vec<u8>,
    pub nodes: usize,
    pub meshes: usize,
    pub triangles: usize,
}

fn num(v: f32) -> Value {
    json!(v as f64)
}

fn normals(pos: &[[f32; 3]], tris: &[[u32; 3]]) -> Vec<[f32; 3]> {
    let mut n = vec![[0f64; 3]; pos.len()];
    for t in tris {
        let [a, b, c] = t.map(|i| pos[i as usize].map(f64::from));
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let f = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        for i in t {
            for k in 0..3 {
                n[*i as usize][k] += f[k];
            }
        }
    }
    n.iter()
        .map(|v| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            if l > 0.0 && l.is_finite() {
                [(v[0] / l) as f32, (v[1] / l) as f32, (v[2] / l) as f32]
            } else {
                [0.0, 1.0, 0.0]
            }
        })
        .collect()
}

/// Items must be usable (`Body::usable`) and sorted by key; the result is a pure function of them.
pub fn write(items: &[Item]) -> Result<Built, String> {
    let mut classes: Vec<&str> = items.iter().map(|i| i.ifc_class.as_str()).collect();
    classes.sort_unstable();
    classes.dedup();
    let materials: Vec<Value> = classes
        .iter()
        .map(|c| {
            let col = colour(c);
            json!({"name": c, "pbrMetallicRoughness": {"baseColorFactor": col.map(num), "metallicFactor": 0.0, "roughnessFactor": 0.85},
                "alphaMode": if col[3] < 1.0 { "BLEND" } else { "OPAQUE" }, "doubleSided": true})
        })
        .collect();

    let mut bin: Vec<u8> = vec![];
    let (mut views, mut accessors, mut meshes, mut nodes): (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) = Default::default();
    let mut shared: HashMap<[u8; 32], usize> = HashMap::new();
    let mut triangles = 0usize;
    for it in items {
        let b = &it.body;
        let lo = b.bbox().map(|v| v as f32);
        let origin = [lo[0], lo[1], lo[2]];
        let local: Vec<[f32; 3]> = b.positions.iter().map(|p| [p[0] - origin[0], p[1] - origin[1], p[2] - origin[2]]).collect();
        let material = classes.binary_search(&it.ifc_class.as_str()).map_err(|_| "class lost".to_string())?;
        let mut h = Sha256::new();
        h.update((material as u64).to_le_bytes());
        for p in &local {
            for c in p {
                h.update(((*c as f64 * 10_000.0).round() as i64).to_le_bytes());
            }
        }
        for t in &b.tris {
            for i in t {
                h.update(i.to_le_bytes());
            }
        }
        let sig: [u8; 32] = h.finalize().into();
        let mesh = match shared.get(&sig) {
            Some(&m) => m,
            None => {
                let nrm = normals(&local, &b.tris);
                let (mut mn, mut mx) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
                let mut add_view = |bin: &mut Vec<u8>, bytes: Vec<u8>, target: u32| {
                    let start = bin.len();
                    bin.extend_from_slice(&bytes);
                    views.push(json!({"buffer": 0, "byteOffset": start, "byteLength": bytes.len(), "target": target}));
                    views.len() - 1
                };
                let mut pb = Vec::with_capacity(local.len() * 12);
                for p in &local {
                    for k in 0..3 {
                        mn[k] = mn[k].min(p[k]);
                        mx[k] = mx[k].max(p[k]);
                        pb.extend_from_slice(&p[k].to_le_bytes());
                    }
                }
                let pv = add_view(&mut bin, pb, 34962);
                accessors.push(json!({"bufferView": pv, "componentType": 5126, "count": local.len(), "type": "VEC3", "min": mn.map(num), "max": mx.map(num)}));
                let pa = accessors.len() - 1;
                let nb: Vec<u8> = nrm.iter().flat_map(|n| n.iter().flat_map(|c| c.to_le_bytes())).collect();
                let nv = add_view(&mut bin, nb, 34962);
                accessors.push(json!({"bufferView": nv, "componentType": 5126, "count": nrm.len(), "type": "VEC3"}));
                let na = accessors.len() - 1;
                let ib: Vec<u8> = b.tris.iter().flat_map(|t| t.iter().flat_map(|i| i.to_le_bytes())).collect();
                let iv = add_view(&mut bin, ib, 34963);
                accessors.push(json!({"bufferView": iv, "componentType": 5125, "count": b.tris.len() * 3, "type": "SCALAR"}));
                meshes.push(json!({"primitives": [{"attributes": {"POSITION": pa, "NORMAL": na}, "indices": accessors.len() - 1, "material": material, "mode": 4}]}));
                shared.insert(sig, meshes.len() - 1);
                meshes.len() - 1
            }
        };
        triangles += b.tris.len();
        let mut n = Map::new();
        n.insert("name".into(), json!(it.key));
        n.insert("mesh".into(), json!(mesh));
        if origin != [0.0; 3] {
            n.insert("translation".into(), json!(origin.map(num)));
        }
        n.insert("extras".into(), json!({"element_key": it.key, "type_key": it.type_key, "ifc_class": it.ifc_class}));
        nodes.push(Value::Object(n));
    }

    let mesh_count = meshes.len();
    let mut root = json!({
        "asset": {"version": "2.0", "generator": "DDC Editor"},
        "scene": 0,
        "scenes": [{"nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes,
    });
    if mesh_count > 0 {
        root["materials"] = Value::Array(materials);
        root["meshes"] = Value::Array(meshes);
        root["accessors"] = Value::Array(accessors);
        root["bufferViews"] = Value::Array(views);
        root["buffers"] = json!([{"byteLength": bin.len()}]);
    }
    let mut js = serde_json::to_vec(&root).map_err(|e| e.to_string())?;
    while js.len() % 4 != 0 {
        js.push(b' ');
    }
    let total = 12 + 8 + js.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let total32 = u32::try_from(total).map_err(|_| "the geometry is over 4 GiB".to_string())?;
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
    Ok(Built { glb: out, nodes: items.len(), meshes: mesh_count, triangles })
}

/// Reads back the JSON chunk of a GLB (for checks).
pub fn json_chunk(glb: &[u8]) -> Result<Value, String> {
    if glb.len() < 20 || &glb[0..4] != b"glTF" {
        return Err("not a GLB".into());
    }
    let n = u32::from_le_bytes([glb[12], glb[13], glb[14], glb[15]]) as usize;
    serde_json::from_slice(glb.get(20..20 + n).ok_or("short GLB")?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(z: f32) -> Body {
        let mut b = Body::default();
        b.add(&[[0., 0., z], [1., 0., z], [1., 1., z], [0., 1., z]], &[[0, 1, 2], [0, 2, 3]], &|p| p, false);
        b
    }

    #[test]
    fn hash_ignores_triangle_order_and_rotation() {
        let a = quad(0.0);
        let h1 = geometry_hash(&a.positions, &a.tris);
        let rot: Vec<[u32; 3]> = a.tris.iter().rev().map(|t| [t[1], t[2], t[0]]).collect();
        assert_eq!(h1, geometry_hash(&a.positions, &rot));
        assert_eq!(h1.len(), 64);
        let flipped: Vec<[u32; 3]> = a.tris.iter().map(|t| [t[0], t[2], t[1]]).collect();
        assert_ne!(h1, geometry_hash(&a.positions, &flipped), "the winding is part of the shape");
    }

    #[test]
    fn hash_rounds_to_whole_millimetres_half_away_from_zero() {
        let t = [[0u32, 1, 2]];
        let p = |x: f32| [[x, 0., 0.], [1., 0., 0.], [1., 1., 0.]];
        assert_eq!(geometry_hash(&p(0.0004), &t), geometry_hash(&p(0.0), &t));
        assert_ne!(geometry_hash(&p(0.0006), &t), geometry_hash(&p(0.0), &t));
        assert_eq!(geometry_hash(&p(0.0006), &t), geometry_hash(&p(0.001), &t));
        assert_eq!(geometry_hash(&p(-0.0006), &t), geometry_hash(&p(-0.001), &t));
    }

    #[test]
    fn hash_moves_with_the_place() {
        let (a, b) = (quad(0.0), quad(0.5));
        assert_ne!(geometry_hash(&a.positions, &a.tris), geometry_hash(&b.positions, &b.tris));
    }

    #[test]
    fn equal_shapes_share_a_mesh_and_nodes_carry_three_extras() {
        let it = |k: &str, z: f32| Item { key: k.into(), type_key: "wall/x".into(), ifc_class: "IfcWall".into(), body: quad(z) };
        let items = vec![it("a", 0.0), it("b", 3.0), it("c", 6.0)];
        let g = write(&items).unwrap();
        assert_eq!((g.nodes, g.meshes, g.triangles), (3, 1, 6));
        let j = json_chunk(&g.glb).unwrap();
        let nodes = j["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 3);
        for (n, k) in nodes.iter().zip(["a", "b", "c"]) {
            assert_eq!(n["name"], k);
            let e = n["extras"].as_object().unwrap();
            assert_eq!(e.keys().cloned().collect::<Vec<_>>(), ["element_key", "ifc_class", "type_key"]);
        }
        assert_eq!(j["meshes"][0]["primitives"][0]["attributes"]["NORMAL"], 1);
        assert_eq!(write(&items).unwrap().glb, g.glb);
    }

    #[test]
    fn empty_scene_is_a_valid_glb() {
        let g = write(&[]).unwrap();
        assert_eq!((g.nodes, g.meshes), (0, 0));
        assert!(json_chunk(&g.glb).unwrap()["nodes"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unusable_bodies_are_flagged() {
        let mut b = quad(0.0);
        assert!(b.usable());
        b.positions[0][0] = f32::NAN;
        assert!(!b.usable());
        assert!(!Body::default().usable());
    }

    #[test]
    fn model_up_becomes_glb_up() {
        assert_eq!(to_glb([1.0, 2.0, 3.0]), [1.0, 3.0, -2.0]);
    }
}
