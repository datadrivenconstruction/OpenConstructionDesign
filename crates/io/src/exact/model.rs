//! A model as an exact exchange document: typed solids from their forms, freeform patches as
//! NURBS faces (a shell as two offset faces and ruled sides), every other element from its
//! display mesh as a polyhedron; one assembly node per level.

use std::collections::{BTreeMap, HashMap};

use ddc_geom::math::v3;
use ddc_geom::mesh::Mesh;
use ddc_geom::nurbs::Surface as NSurf;
use ddc_model::{FreeformSpec, Model};

use super::build::{self, Opts};
use super::{Ax3, Body, Doc, Node, Part};
use crate::meshx::{category_rgba, linear_to_srgb8, MeshSet};

/// What the document holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    /// Solids written with exact faces.
    pub exact: usize,
    /// Elements written from their display mesh (planar faces).
    pub faceted: usize,
    /// One line per solid that could not be written exactly, with the reason.
    pub notes: Vec<String>,
}

fn rgb(c: [f32; 4]) -> [u8; 3] {
    [linear_to_srgb8(c[0]), linear_to_srgb8(c[1]), linear_to_srgb8(c[2])]
}

fn mesh_of(pos: &[[f64; 3]], tri: &[[u32; 3]]) -> Mesh {
    Mesh {
        positions: pos.iter().map(|p| v3(p[0], p[1], p[2])).collect(),
        triangles: tri.to_vec(),
        face_of: vec![0; tri.len()],
    }
}

/// Largest distance of a written shell face from the true offset of its patch, m.
pub const OFFSET_TOL: f64 = 5e-4;

/// The exact body of a freeform patch in world coordinates (`z`: its level elevation).
/// Refused for a trimmed patch: its trim loops are parameter polylines with no exact edge.
pub fn freeform_body(s: &FreeformSpec, z: f64) -> Result<super::Body, String> {
    if !s.trim.is_empty() {
        return Err("trimmed patch: the trim loops are parameter polylines, not exact edges".into());
    }
    let f = &s.surface;
    let ctrl = f.ctrl().iter().map(|p| v3(p.x, p.y, p.z + z)).collect();
    let lifted = NSurf::new(
        f.deg_u(),
        f.deg_v(),
        f.knots_u().to_vec(),
        f.knots_v().to_vec(),
        f.nu(),
        f.nv(),
        ctrl,
        f.weights().to_vec(),
    )
    .map_err(|e| e.to_string())?;
    if s.thickness > 0.0 {
        let (a, b) = s.side.offsets(s.thickness);
        Ok(super::nurbs_face::shell(&lifted, a, b, OFFSET_TOL)?.0)
    } else {
        super::nurbs_face::sheet(&lifted)
    }
}

/// Adds the model's solids (voids left out) as `solid:<id>` parts and its freeform patches
/// as `freeform:<id>` parts, for a set read from the model tables, which has neither.
pub fn add_solids(m: &Model, set: &mut MeshSet) -> Result<(), String> {
    let err = |e: ddc_model::Error| e.to_string();
    let levels: HashMap<i64, String> = m.level_list().map_err(err)?.into_iter().map(|l| (l.0, l.1)).collect();
    let mat = set.materials.len();
    let mut used = false;
    for (id, s) in m.solids(None).map_err(err)? {
        if s.void || set.parts.iter().any(|p| p.key == format!("solid:{id}")) {
            continue;
        }
        let mesh = m.solid_mesh(id).map_err(err)?;
        used = true;
        set.parts.push(crate::meshx::Part {
            key: format!("solid:{id}"),
            name: if s.name.trim().is_empty() { format!("solid {id}") } else { s.name.clone() },
            category: "solid".into(),
            level: levels.get(&s.level).cloned(),
            material: Some(mat),
            positions: mesh.positions.iter().map(|p| [p.x, p.y, p.z]).collect(),
            triangles: mesh.triangles.clone(),
        });
    }
    if used {
        set.materials.push(crate::meshx::Material {
            name: "solid".into(),
            rgba: category_rgba("solid"),
        });
    }
    add_freeforms(m, set)
}

fn add_freeforms(m: &Model, set: &mut MeshSet) -> Result<(), String> {
    let err = |e: ddc_model::Error| e.to_string();
    let levels: HashMap<i64, String> = m.level_list().map_err(err)?.into_iter().map(|l| (l.0, l.1)).collect();
    let mat = set.materials.len();
    let mut used = false;
    for (id, s) in m.freeforms(None).map_err(err)? {
        let key = format!("freeform:{id}");
        if set.parts.iter().any(|p| p.key == key) {
            continue;
        }
        let mesh = m.freeform_mesh(id).map_err(err)?.mesh;
        used = true;
        set.parts.push(crate::meshx::Part {
            key,
            name: if s.name.trim().is_empty() { format!("freeform {id}") } else { s.name.clone() },
            category: "freeform".into(),
            level: levels.get(&s.level).cloned(),
            material: Some(mat),
            positions: mesh.positions.iter().map(|p| [p.x, p.y, p.z]).collect(),
            triangles: mesh.triangles.clone(),
        });
    }
    if used {
        set.materials.push(crate::meshx::Material {
            name: "freeform".into(),
            rgba: category_rgba("freeform"),
        });
    }
    Ok(())
}

/// [`doc`] written as a STEP file.
pub fn write_step(
    m: &Model,
    set: &MeshSet,
    all_solids: bool,
    title: &str,
    w: &crate::step::write::WriteOptions,
    o: &Opts,
    path: &std::path::Path,
) -> Result<Report, String> {
    let (d, rep) = doc(m, set, all_solids, title, o)?;
    let text = crate::step::write::write(&d, w)?;
    crate::fsutil::replace_durably_bytes(path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(rep)
}

/// The parts of `set`, a part with a `solid:<id>` key built from that solid's form and one
/// with a `freeform:<id>` key from that exact patch; with `all_solids` the model's other
/// solids and freeform patches too (a set from the model tables has neither).
pub fn doc(m: &Model, set: &MeshSet, all_solids: bool, title: &str, o: &Opts) -> Result<(Doc, Report), String> {
    let err = |e: ddc_model::Error| e.to_string();
    let elev: HashMap<i64, (String, f64)> = m.level_list().map_err(err)?.into_iter().map(|l| (l.0, (l.1, l.2))).collect();
    let solids: BTreeMap<i64, ddc_model::SolidSpec> = m.solids(None).map_err(err)?.into_iter().filter(|s| !s.1.void).collect();
    let mut rep = Report::default();
    let mut parts: Vec<(Option<String>, Part)> = vec![];
    let exact_solid = |id: i64, name: &str, rgb: [u8; 3], rep: &mut Report| -> Result<Option<(Option<String>, Part)>, String> {
        let Some(s) = solids.get(&id) else { return Ok(None) };
        let (level, z) = elev.get(&s.level).cloned().unwrap_or_default();
        let level = (!level.is_empty()).then_some(level);
        let cut = m.solid_quantities(id).map_err(err)?.is_some_and(|q| q.net_volume < q.volume * (1.0 - 1e-9));
        let body = if cut {
            rep.notes.push(format!("solid {id} ({}): cut by voids, written from its mesh", s.form.kind()));
            rep.faceted += 1;
            build::polyhedron(&m.solid_mesh(id).map_err(err)?)?
        } else {
            let mut fr = s.frame;
            fr.o.z += z;
            let (b, why) = build::form_with(&s.form, &fr, o).map_err(|e| format!("solid {id}: {e}"))?;
            match why {
                None => rep.exact += 1,
                Some(w) => {
                    rep.notes.push(format!("solid {id} ({}): {w}; written as a polyhedron", s.form.kind()));
                    rep.faceted += 1;
                }
            }
            b
        };
        Ok(Some((
            level,
            Part {
                name: name.to_string(),
                body,
                rgb: Some(rgb),
            },
        )))
    };
    let freeforms: BTreeMap<i64, FreeformSpec> = m.freeforms(None).map_err(err)?.into_iter().collect();
    let exact_freeform = |id: i64, name: &str, rgb: [u8; 3], rep: &mut Report| -> Result<Option<(Option<String>, Part)>, String> {
        let Some(s) = freeforms.get(&id) else { return Ok(None) };
        let (level, z) = elev.get(&s.level).cloned().unwrap_or_default();
        let level = (!level.is_empty()).then_some(level);
        let body = match freeform_body(s, z) {
            Ok(b) => {
                rep.exact += 1;
                b
            }
            // only a trimmed patch has no exact boundary; any other failure is an error
            Err(why) if !s.trim.is_empty() => {
                rep.notes.push(format!("freeform {id} ({}): {why}; written from its mesh", s.origin));
                rep.faceted += 1;
                build::polyhedron(&m.freeform_mesh(id).map_err(err)?.mesh)?
            }
            Err(why) => return Err(format!("freeform {id} ({}): {why}", s.origin)),
        };
        Ok(Some((
            level,
            Part {
                name: name.to_string(),
                body,
                rgb: Some(rgb),
            },
        )))
    };
    let mut seen = std::collections::HashSet::new();
    let mut seen_ff = std::collections::HashSet::new();
    for p in &set.parts {
        let colour = p.material.and_then(|i| set.materials.get(i)).map(|x| rgb(x.rgba));
        if let Some(id) = p.key.strip_prefix("freeform:").and_then(|s| s.parse::<i64>().ok()) {
            seen_ff.insert(id);
            let c = colour.unwrap_or_else(|| rgb(category_rgba("freeform")));
            if let Some(x) = exact_freeform(id, &p.name, c, &mut rep)? {
                parts.push(x);
                continue;
            }
        }
        let solid = p.key.strip_prefix("solid:").and_then(|s| s.parse::<i64>().ok());
        if let Some(id) = solid {
            seen.insert(id);
            let c = colour.unwrap_or_else(|| rgb(category_rgba("solid")));
            if let Some(x) = exact_solid(id, &p.name, c, &mut rep)? {
                parts.push(x);
                continue;
            }
        }
        let body: Body = build::polyhedron(&mesh_of(&p.positions, &p.triangles))?;
        rep.faceted += 1;
        parts.push((
            p.level.clone(),
            Part {
                name: p.name.clone(),
                body,
                rgb: colour,
            },
        ));
    }
    if all_solids {
        let ids: Vec<i64> = solids.keys().copied().filter(|id| !seen.contains(id)).collect();
        for id in ids {
            let name = match solids[&id].name.trim() {
                "" => format!("solid {id}"),
                n => n.to_string(),
            };
            if let Some(x) = exact_solid(id, &name, rgb(category_rgba("solid")), &mut rep)? {
                parts.push(x);
            }
        }
        let ids: Vec<i64> = freeforms.keys().copied().filter(|id| !seen_ff.contains(id)).collect();
        for id in ids {
            let name = match freeforms[&id].name.trim() {
                "" => format!("freeform {id}"),
                n => n.to_string(),
            };
            if let Some(x) = exact_freeform(id, &name, rgb(category_rgba("freeform")), &mut rep)? {
                parts.push(x);
            }
        }
    }
    if parts.is_empty() {
        return Err("nothing to export: no 3D body".into());
    }
    let mut doc = Doc {
        name: title.to_string(),
        parts: vec![],
        roots: vec![],
    };
    let mut by_level: BTreeMap<String, Vec<Node>> = BTreeMap::new();
    for (level, part) in parts {
        let i = doc.parts.len();
        let leaf = Node::leaf(&part.name, Ax3::WORLD, i);
        doc.parts.push(part);
        match level {
            Some(l) => by_level.entry(l).or_default().push(leaf),
            None => doc.roots.push(leaf),
        }
    }
    for (name, children) in by_level {
        doc.roots.push(Node {
            name,
            place: Ax3::WORLD,
            part: None,
            children,
        });
    }
    Ok((doc, rep))
}
