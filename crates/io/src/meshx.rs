// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Neutral triangle exchange (docs/design/IO_FORMATS.md): every mesh writer (STL, OBJ, DXF)
//! takes a [`MeshSet`], every mesh reader (glTF, STEP, STL, OBJ) returns one, and
//! [`import_into`] is the only way such a set enters the model.

use std::collections::{HashMap, HashSet};

use ddc_model::Model;
use rusqlite::{params, OptionalExtension};

use crate::job::{Progress, CANCELLED};

/// Category of imported parts a reader cannot classify.
pub const GENERIC: &str = "Generic Model";

#[derive(Clone, Debug, PartialEq)]
pub struct Material {
    pub name: String,
    /// Linear RGBA, 0..1.
    pub rgba: [f32; 4],
}

/// One element: world positions in metres, Z up; triangles counter-clockwise from outside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// `<table>:<id>` for a model element; a source-stable key for a read part.
    pub key: String,
    pub name: String,
    pub category: String,
    pub level: Option<String>,
    pub material: Option<usize>,
    pub positions: Vec<[f64; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

impl Part {
    pub fn bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        bbox_of(self.triangles.iter().flatten().map(|&i| self.positions[i as usize]))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshSet {
    pub materials: Vec<Material>,
    pub parts: Vec<Part>,
}

fn bbox_of(pts: impl Iterator<Item = [f64; 3]>) -> Option<([f64; 3], [f64; 3])> {
    let mut b: Option<([f64; 3], [f64; 3])> = None;
    for p in pts {
        let e = b.get_or_insert((p, p));
        for k in 0..3 {
            e.0[k] = e.0[k].min(p[k]);
            e.1[k] = e.1[k].max(p[k]);
        }
    }
    b
}

/// Linear 0..1 to 8-bit sRGB.
pub fn linear_to_srgb8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// 8-bit sRGB to linear 0..1.
pub fn srgb8_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

impl MeshSet {
    /// Material by name; a repeated name returns the first one.
    pub fn material(&mut self, name: &str, rgba: [f32; 4]) -> usize {
        if let Some(i) = self.materials.iter().position(|m| m.name == name) {
            return i;
        }
        self.materials.push(Material {
            name: name.into(),
            rgba,
        });
        self.materials.len() - 1
    }

    /// Adds a part with at least one valid triangle; triangles with an index out of range or
    /// a non-finite corner are dropped. Returns false when nothing was left to add.
    pub fn push(&mut self, mut p: Part) -> bool {
        let n = p.positions.len();
        let ok = |t: &[u32; 3], pos: &[[f64; 3]]| {
            t.iter().all(|&i| (i as usize) < n)
                && t.iter().all(|&i| pos[i as usize].iter().all(|c| c.is_finite()))
        };
        let pos = std::mem::take(&mut p.positions);
        p.triangles.retain(|t| ok(t, &pos));
        p.positions = pos;
        if p.triangles.is_empty() {
            return false;
        }
        if p.material.is_some_and(|m| m >= self.materials.len()) {
            p.material = None;
        }
        self.parts.push(p);
        true
    }

    pub fn triangle_count(&self) -> usize {
        self.parts.iter().map(|p| p.triangles.len()).sum()
    }

    /// Bounding box of every used vertex.
    pub fn bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        bbox_of(self.parts.iter().flat_map(|p| {
            p.triangles
                .iter()
                .flatten()
                .map(move |&i| p.positions[i as usize])
        }))
    }

    /// Only the parts whose key is in `keys`; materials stay as they are.
    pub fn only(&self, keys: &HashSet<String>) -> MeshSet {
        MeshSet {
            materials: self.materials.clone(),
            parts: self
                .parts
                .iter()
                .filter(|p| keys.contains(&p.key))
                .cloned()
                .collect(),
        }
    }

    /// Flattens a glTF export scene: node placement applied, element key from the node extras
    /// (`ddc.table`/`ddc.id`), several nodes of one element merged into one part.
    pub fn from_gltf(s: &crate::gltf::Scene) -> MeshSet {
        let mut out = MeshSet::default();
        for m in &s.materials {
            out.material(&m.name, m.color);
        }
        let mut by_key: HashMap<String, usize> = HashMap::new();
        for (ni, n) in s.nodes.iter().enumerate() {
            let Some(mesh) = s.meshes.get(n.mesh) else {
                continue;
            };
            let d = &n.extras["ddc"];
            let key = match (d["table"].as_str(), d["id"].as_i64()) {
                (Some(t), Some(id)) => format!("{t}:{id}"),
                _ => format!("node:{ni}"),
            };
            let place = |p: &[f32; 3]| -> [f64; 3] {
                let v = p.map(f64::from);
                match &n.matrix {
                    Some(m) => {
                        let m = m.map(f64::from);
                        [0, 1, 2].map(|r| m[r] * v[0] + m[4 + r] * v[1] + m[8 + r] * v[2] + m[12 + r])
                    }
                    None => [0, 1, 2].map(|k| v[k] + f64::from(n.translation[k])),
                }
            };
            let flip = n.matrix.is_some_and(|m| det3(&m) < 0.0);
            let i = *by_key.entry(key.clone()).or_insert_with(|| {
                out.parts.push(Part {
                    key,
                    name: n.name.clone(),
                    category: d["category"].as_str().unwrap_or(GENERIC).to_string(),
                    level: d["level"].as_str().map(str::to_string),
                    material: Some(mesh.material).filter(|&m| m < s.materials.len()),
                    ..Default::default()
                });
                out.parts.len() - 1
            });
            let p = &mut out.parts[i];
            let base = p.positions.len() as u32;
            p.positions.extend(mesh.positions.iter().map(place));
            p.triangles.extend(mesh.triangles.iter().map(|t| {
                let t = if flip { [t[0], t[2], t[1]] } else { *t };
                t.map(|k| k + base)
            }));
        }
        out.parts.retain(|p| !p.triangles.is_empty());
        out
    }

    /// Model-level bodies, for callers without the editor (the `ddc` CLI): imported meshes,
    /// layered bodies after joins, ceilings and MEP runs. Returns the set and warnings, one of
    /// them counting the elements that only the editor can tessellate.
    pub fn from_model(m: &Model) -> Result<(MeshSet, Vec<String>), String> {
        let e = |e: ddc_model::Error| e.to_string();
        let mut out = MeshSet::default();
        let mut warns = vec![];
        let levels: HashMap<i64, String> = m
            .level_list()
            .map_err(e)?
            .into_iter()
            .map(|l| (l.0, l.1))
            .collect();
        let db = m.sql();
        let level_of = |table: &str, id: i64| -> Option<String> {
            let lv: Option<i64> = db
                .query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| r.get(0))
                .optional()
                .ok()
                .flatten()
                .flatten();
            lv.and_then(|l| levels.get(&l).cloned())
        };
        let ids = |table: &str| -> Vec<i64> {
            let Ok(mut st) = db.prepare(&format!("SELECT id FROM {table} ORDER BY id")) else {
                return vec![];
            };
            st.query_map([], |r| r.get(0))
                .map(|r| r.filter_map(Result::ok).collect())
                .unwrap_or_default()
        };
        let add = |out: &mut MeshSet, table: &str, id: i64, cat: &str, mesh: &ddc_geom::mesh::Mesh, name: String| {
            let mat = out.material(cat, category_rgba(cat));
            out.push(Part {
                key: format!("{table}:{id}"),
                name,
                category: cat.to_string(),
                level: level_of(table, id),
                material: Some(mat),
                positions: mesh.positions.iter().map(|v| [v.x, v.y, v.z]).collect(),
                triangles: mesh.triangles.clone(),
            })
        };
        for im in m.imported_meshes(None).map_err(e)? {
            let cat = if im.category.trim().is_empty() {
                GENERIC.to_string()
            } else {
                im.category.clone()
            };
            add(&mut out, "mesh", im.id, &cat, &im.mesh, im.name.clone());
        }
        let bodies = [
            ("wall", "wall"),
            ("slab", "floor"),
            ("roof", "roof"),
            ("col", "column"),
            ("beam", "beam"),
            ("curtain_wall", "curtain"),
            ("mass", "mass"),
        ];
        let mut failed = 0usize;
        for (table, cat) in bodies {
            for id in ids(table) {
                let parts = match m.joined_parts(table, id) {
                    Ok(p) => p,
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                };
                let mut mesh = ddc_geom::mesh::Mesh {
                    positions: vec![],
                    triangles: vec![],
                    face_of: vec![],
                };
                for (_, b) in parts {
                    let pm = b.to_mesh();
                    let base = mesh.positions.len() as u32;
                    mesh.positions.extend(pm.positions);
                    mesh.triangles.extend(pm.triangles.iter().map(|t| t.map(|k| k + base)));
                }
                if !add(&mut out, table, id, cat, &mesh, format!("{cat} {id}")) {
                    failed += 1;
                }
            }
        }
        for id in ids("ceiling") {
            match m.ceiling_mesh(id) {
                Ok(mesh) if add(&mut out, "ceiling", id, "ceiling", &mesh, format!("ceiling {id}")) => {}
                _ => failed += 1,
            }
        }
        for (id, _, mesh) in m.mep_meshes(None).map_err(e)? {
            add(&mut out, "mep_run", id, "mep", &mesh, format!("mep {id}"));
        }
        // solids and freeform patches are model bodies too: every mesh export carries them
        crate::exact::model::add_solids(m, &mut out)?;
        if failed > 0 {
            warns.push(format!("{failed} element(s) have no body to export"));
        }
        let editor_only: usize = ["opening", "comp", "stair", "railing", "ramp"]
            .iter()
            .map(|t| ids(t).len())
            .sum();
        if editor_only > 0 {
            warns.push(format!(
                "{editor_only} door, window, component, stair, railing or ramp element(s) are \
                 tessellated only by the editor: use `ddc-app --file <in> --export <out>` for them"
            ));
        }
        Ok((out, warns))
    }
}

/// Extensions [`read_file`] takes.
pub const READ_EXTS: [&str; 6] = ["glb", "gltf", "step", "stp", "stl", "obj"];

/// Largest extent (in file units) above which a unitless file (STL, OBJ) is read as
/// millimetres: a 500 m part is unlikely, a 500 mm one is common.
const MM_GUESS: f64 = 500.0;

/// Unitless file read in metres, then turned to millimetres when its size says so.
fn guess_units(
    mut set: MeshSet,
    mut warns: Vec<String>,
    name: &str,
) -> (MeshSet, Vec<String>) {
    let Some((lo, hi)) = set.bbox() else {
        return (set, warns);
    };
    let ext = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
    if ext > MM_GUESS {
        for p in &mut set.parts {
            for v in &mut p.positions {
                *v = v.map(|c| c / 1000.0);
            }
        }
        warns.push(format!(
            "{name} has no units; read as millimetres (largest size {ext:.0})"
        ));
    } else {
        warns.push(format!("{name} has no units; read as metres"));
    }
    (set, warns)
}

/// Any mesh file by extension. STL and OBJ carry no units: see [`MM_GUESS`].
pub fn read_file(
    path: &std::path::Path,
    progress: Option<&Progress>,
) -> Result<(MeshSet, Vec<String>), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match ext.as_str() {
        "glb" | "gltf" => crate::gltf::read_file(path, progress),
        "step" | "stp" => crate::step::read_file(path, progress),
        "stl" => {
            let (s, w) = crate::stl::read_file(path, Units::Metre, progress)?;
            Ok(guess_units(s, w, &name))
        }
        "obj" => {
            let o = crate::obj::ObjOptions::default();
            let (s, w) = crate::obj::read_file(path, o, progress)?;
            Ok(guess_units(s, w, &name))
        }
        e => Err(format!(
            "{name}: .{e} is not a mesh format (expected {})",
            READ_EXTS.map(|x| format!(".{x}")).join(", ")
        )),
    }
}

fn det3(m: &[f32; 16]) -> f32 {
    m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
        + m[8] * (m[1] * m[6] - m[5] * m[2])
}

/// Neutral colours (linear) per category for sets without their own materials.
pub fn category_rgba(cat: &str) -> [f32; 4] {
    let c = match cat {
        "wall" => [0.80, 0.79, 0.76],
        "door" => [0.55, 0.38, 0.24],
        "window" | "curtain" => [0.62, 0.80, 0.90],
        "floor" | "ceiling" => [0.65, 0.65, 0.67],
        "roof" => [0.55, 0.35, 0.30],
        _ => [0.70, 0.70, 0.70],
    };
    [c[0], c[1], c[2], 1.0]
}

/// Length units of a mesh file without its own unit (STL, OBJ).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Units {
    #[default]
    Metre,
    Millimetre,
}

impl Units {
    /// File units per metre.
    pub fn per_metre(self) -> f64 {
        match self {
            Units::Metre => 1.0,
            Units::Millimetre => 1000.0,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "m" | "metre" | "meter" | "metres" | "meters" => Some(Units::Metre),
            "mm" | "millimetre" | "millimeter" | "millimetres" | "millimeters" => Some(Units::Millimetre),
            _ => None,
        }
    }
}

/// What [`import_into`] wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportStats {
    pub meshes: usize,
    pub triangles: usize,
    pub materials: usize,
    /// Mesh row ids, in part order.
    pub ids: Vec<i64>,
}

fn rgb_int(rgba: [f32; 4]) -> i64 {
    let c = rgba.map(linear_to_srgb8);
    (i64::from(c[0]) << 16) | (i64::from(c[1]) << 8) | i64::from(c[2])
}

/// Writes `set` as generic meshes in one undo step. Each part goes to the highest level at or
/// below its lowest point (the lowest level when below all; no level in a model without
/// levels), keeps its name, gets its material as an element material, and props
/// `Source` (file name), `SourceKey`, `SourceName`. Cancelling through `progress` before the
/// commit leaves the model as it was.
pub fn import_into(
    m: &mut Model,
    set: &MeshSet,
    source: &str,
    progress: Option<&Progress>,
) -> Result<ImportStats, String> {
    if set.parts.is_empty() {
        return Err(format!("{source}: the file has no triangles to import"));
    }
    let mut levels: Vec<(i64, f64)> = m
        .level_list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|l| (l.0, l.2))
        .collect();
    levels.sort_by(|a, b| a.1.total_cmp(&b.1));
    let level_for = |zmin: f64| -> Option<i64> {
        levels
            .iter()
            .rev()
            .find(|l| l.1 <= zmin + 1e-6)
            .or(levels.first())
            .map(|l| l.0)
    };
    if let Some(p) = progress {
        p.phase("job.stage.commit", 0.8, 1.0);
        p.set_bytes(false);
        p.set_total(set.parts.len() as u64);
        p.set(0);
        if p.is_cancelled() {
            return Err(CANCELLED.into());
        }
    }
    let used: Vec<bool> = (0..set.materials.len())
        .map(|i| set.parts.iter().any(|p| p.material == Some(i)))
        .collect();
    let res = m.batch_walls_edit("wall_import_mesh", |db, snaps| {
        snaps.watermark(db, "material")?;
        snaps.watermark(db, "mesh_material")?;
        let mut st = ImportStats::default();
        let mut mat_names = vec![];
        for (i, mat) in set.materials.iter().enumerate() {
            let name: String = mat.name.trim().chars().take(120).collect();
            let name = if name.is_empty() {
                format!("Material {}", i + 1)
            } else {
                name
            };
            if used[i] {
                let have: Option<i64> = db
                    .query_row("SELECT id FROM material WHERE name=?1", [&name], |r| r.get(0))
                    .optional()?;
                if have.is_none() {
                    db.execute(
                        "INSERT INTO material(name,color,hatch,note) VALUES (?1,?2,?3,'')",
                        params![name, rgb_int(mat.rgba), ddc_model::material_default_hatch(&name)],
                    )?;
                    st.materials += 1;
                }
            }
            mat_names.push(name);
        }
        for (k, p) in set.parts.iter().enumerate() {
            if progress.is_some_and(|p| p.is_cancelled()) {
                return Err(ddc_model::Error::Invalid(CANCELLED.into()));
            }
            let zmin = p.bbox().map_or(0.0, |b| b.0[2]);
            let mut pb = Vec::with_capacity(p.positions.len() * 24);
            for v in &p.positions {
                for c in v {
                    pb.extend_from_slice(&c.to_le_bytes());
                }
            }
            let ib: Vec<u8> = p.triangles.iter().flatten().flat_map(|i| i.to_le_bytes()).collect();
            let cat = if p.category.trim().is_empty() {
                GENERIC
            } else {
                p.category.as_str()
            };
            let name = if p.name.trim().is_empty() {
                format!("{cat} {}", k + 1)
            } else {
                p.name.clone()
            };
            db.execute(
                "INSERT INTO mesh(level,category,name,positions,indices) VALUES (?1,?2,?3,?4,?5)",
                params![level_for(zmin), cat, name, pb, ib],
            )?;
            let id = db.last_insert_rowid();
            for (key, val) in [("Source", source), ("SourceKey", p.key.as_str()), ("SourceName", p.name.as_str())] {
                if !val.is_empty() {
                    db.execute(
                        "INSERT OR REPLACE INTO prop(kind,ref,key,value,unit) VALUES ('mesh',?1,?2,?3,'')",
                        params![id, key, val],
                    )?;
                }
            }
            if let Some(mi) = p.material {
                db.execute(
                    "INSERT INTO mesh_material(scope,key,material) VALUES ('element',?1,?2)",
                    params![id.to_string(), mat_names[mi]],
                )?;
            }
            st.meshes += 1;
            st.triangles += p.triangles.len();
            st.ids.push(id);
            if let Some(p) = progress {
                p.set(k as u64 + 1);
            }
        }
        Ok(st)
    });
    match res {
        Ok(st) => {
            if let Some(p) = progress {
                p.finish();
            }
            Ok(st)
        }
        Err(ddc_model::Error::Invalid(s)) if s == CANCELLED => Err(CANCELLED.into()),
        Err(e) => Err(format!("{source}: could not write the meshes: {e}")),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn cube(key: &str, at: [f64; 3]) -> Part {
        let mut p = Part {
            key: key.into(),
            name: key.into(),
            category: "box".into(),
            ..Default::default()
        };
        for i in 0..8u32 {
            p.positions.push([
                at[0] + f64::from(i & 1),
                at[1] + f64::from((i >> 1) & 1),
                at[2] + f64::from((i >> 2) & 1),
            ]);
        }
        p.triangles = vec![
            [0, 2, 1], [1, 2, 3], [4, 5, 6], [5, 7, 6], [0, 1, 4], [1, 5, 4],
            [2, 6, 3], [3, 6, 7], [0, 4, 2], [2, 4, 6], [1, 3, 5], [3, 7, 5],
        ];
        p
    }

    pub(crate) fn two_cubes() -> MeshSet {
        let mut s = MeshSet::default();
        let red = s.material("red", [1.0, 0.0, 0.0, 1.0]);
        let mut a = cube("wall:1", [0.0, 0.0, 0.0]);
        a.material = Some(red);
        assert!(s.push(a));
        assert!(s.push(cube("mesh:2", [3.0, 0.0, 1.0])));
        s
    }

    #[test]
    fn push_drops_bad_triangles_and_counts() {
        let mut s = two_cubes();
        let mut bad = cube("x", [0.0; 3]);
        bad.triangles = vec![[0, 1, 99]];
        assert!(!s.push(bad));
        assert_eq!(s.triangle_count(), 24);
        assert_eq!(s.bbox(), Some(([0.0, 0.0, 0.0], [4.0, 1.0, 2.0])));
        let only: HashSet<String> = ["mesh:2".to_string()].into();
        assert_eq!(s.only(&only).triangle_count(), 12);
    }

    #[test]
    fn import_writes_one_undo_step_with_materials_and_levels() {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 1.0).unwrap();
        let st = import_into(&mut m, &two_cubes(), "two.glb", None).unwrap();
        assert_eq!((st.meshes, st.triangles, st.materials), (2, 24, 1));
        let ms = m.imported_meshes(None).unwrap();
        assert_eq!(ms[0].level, Some(l0));
        assert_eq!(ms[1].level, Some(l1));
        assert_eq!(ms[0].name, "wall:1");
        let (own, _) = m.mesh_material_of(ms[0].id).unwrap();
        assert_eq!(own.as_deref(), Some("red"));
        let props = m.props("mesh", ms[1].id).unwrap();
        assert!(props.iter().any(|p| p.0 == "SourceKey"));
        m.undo().unwrap();
        assert!(m.imported_meshes(None).unwrap().is_empty());
    }

    #[test]
    fn cancelled_import_leaves_the_model_alone() {
        let mut m = Model::open(":memory:").unwrap();
        let p = Progress::new();
        p.cancel();
        let r = import_into(&mut m, &two_cubes(), "x.stl", Some(&p));
        assert_eq!(r, Err(CANCELLED.to_string()));
        assert!(m.imported_meshes(None).unwrap().is_empty());
    }

    #[test]
    fn empty_set_is_a_clear_error() {
        let mut m = Model::open(":memory:").unwrap();
        let e = import_into(&mut m, &MeshSet::default(), "a.obj", None).unwrap_err();
        assert!(e.contains("no triangles"), "{e}");
    }
}
