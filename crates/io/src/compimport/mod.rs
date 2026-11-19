//! Import wizard backend: model files → meshes by material in model axes (metres, z up), ready
//! for `ddc_geom::component::from_mesh::family_text`. IFC type objects: `crate::ifc::type_drafts`.

use std::path::Path;

pub use ddc_geom::component::from_mesh::{family_text, ImportSpec, Insertion, MeshPart};
use ddc_geom::component::meshbody::MeshBody;
use ddc_geom::math::{v3, V3};

pub mod gltf;
pub mod obj;
pub mod step;
#[cfg(test)]
mod tests;

/// What a reader found: meshes by material and what it could not read.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub name: String,
    pub parts: Vec<MeshPart>,
    pub warnings: Vec<String>,
}

/// Source units and axes; glTF fixes both (metres, y up), OBJ leaves them to the user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axes {
    /// Metres per file unit.
    pub unit: f64,
    pub y_up: bool,
}

impl Axes {
    pub const GLTF: Axes = Axes {
        unit: 1.0,
        y_up: true,
    };
    pub fn map(&self, p: [f64; 3]) -> V3 {
        let q = v3(p[0] * self.unit, p[1] * self.unit, p[2] * self.unit);
        if self.y_up {
            ddc_geom::component::from_mesh::y_up_to_z_up(q)
        } else {
            q
        }
    }
}

/// Collects triangles by material, then turns them into parts.
#[derive(Default)]
pub(crate) struct Collect {
    mats: Vec<(String, Vec<V3>, Vec<[u32; 3]>)>,
}

impl Collect {
    pub fn slot(&mut self, mat: &str) -> usize {
        match self.mats.iter().position(|m| m.0 == mat) {
            Some(k) => k,
            None => {
                self.mats.push((mat.to_string(), vec![], vec![]));
                self.mats.len() - 1
            }
        }
    }
    pub fn add(&mut self, slot: usize, pos: &[V3], tris: impl Iterator<Item = [u32; 3]>) {
        let (_, p, t) = &mut self.mats[slot];
        let base = p.len() as u32;
        p.extend_from_slice(pos);
        t.extend(tris.map(|x| x.map(|i| i + base)));
    }
    pub fn parts(self) -> Vec<MeshPart> {
        self.mats
            .into_iter()
            .filter_map(|(material, p, t)| {
                MeshBody::new(p, t).ok().map(|body| MeshPart { material, body })
            })
            .collect()
    }
}

/// Reads by extension: .obj (with its .mtl names), .gltf/.glb, .step/.stp.
pub fn read_path(path: &Path, axes: Axes) -> Result<Source, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut s = match ext.as_str() {
        "obj" => obj::read(&String::from_utf8_lossy(&bytes), axes)?,
        "glb" | "gltf" => gltf::read(&bytes, path.parent())?,
        "step" | "stp" => step::read(&bytes)?,
        o => return Err(format!("not a model file: .{o}")),
    };
    if s.name.is_empty() {
        s.name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("Imported")
            .replace(['_', '-'], " ");
    }
    if s.parts.is_empty() {
        return Err(format!("{}: no triangles", path.display()));
    }
    // pieces are checked together: one body cut by material closes along shared edges
    let (mut pos, mut tris) = (vec![], vec![]);
    for p in &s.parts {
        let base = pos.len() as u32;
        pos.extend_from_slice(&p.body.pos);
        tris.extend(p.body.tris.iter().map(|t| t.map(|i| i + base)));
    }
    if !MeshBody::new(pos, tris).map(|b| b.welded().closed()).unwrap_or(false) {
        s.warnings.push("the meshes are not closed together: the volume is 0".into());
    }
    Ok(s)
}
