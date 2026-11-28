// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Whole model -> glTF: every level, absolute heights, materials by category, element ids and
//! properties in node extras, repeated bodies as shared meshes. Built level by level from the
//! model, not from the viewport scene: that one identifies only the active level and carries
//! section caps, linked models and clash colours.

use std::collections::{HashMap, HashSet};

use ddc_io::gltf::{json, Props, Scene as Gltf, Value};
use ddc_model::{Lod, MepPart, Model};
use ddc_render::scene::{TAG_GLASS, TAG_RGB, TAG_TRANSLUCENT, TAG_WINDOW};
use ddc_render::style::base_tag;
use ddc_render::{is_translucent, tag_rgb, SceneMesh};
use rusqlite::types::Value as Sql;

use crate::edit3d::pick::{Cat, Target};
use crate::edit3d::scene3d;
use crate::editor::{Editor, Sel};
use crate::phase_ui::sel_ref;
use crate::scene_ids::{EXT_ROOF_EDGE, EXT_SOLID, TOPO_BASE};
use crate::solid_tools::{SOLID_BASE, SOLID_SPAN};
use crate::view3d::scene::mesh_category;

const GLASS_ALPHA: f32 = 0.4;

pub struct Export {
    pub gltf: Gltf,
    /// Bodies that could not be built exactly (or at all), one line each.
    pub issues: Vec<String>,
}

/// Export colours (linear RGB) per category; mep takes the system colour of the tag.
fn category_color(cat: &str) -> [f32; 3] {
    match cat {
        "wall" => [0.80, 0.79, 0.76],
        "door" => [0.55, 0.38, 0.24],
        "window" => [0.62, 0.80, 0.90],
        "curtain" => [0.55, 0.75, 0.85],
        "floor" => [0.65, 0.65, 0.67],
        _ => [0.70, 0.70, 0.70],
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Material for a mesh: by category, MEP by system colour (the tag holds sRGB, glTF wants
/// linear). Only glass and translucent shells are see-through, not a whole category.
fn material(g: &mut Gltf, cat: &str, tag: u32) -> usize {
    let t = base_tag(tag);
    let glass = is_translucent(t);
    let alpha = if glass { GLASS_ALPHA } else { 1.0 };
    if t & TAG_RGB != 0 && t & 0x8000_0000 == 0 {
        let rgb = t & 0x00FF_FFFF;
        let c = |shift: u32| srgb_to_linear(((rgb >> shift) & 255) as f32 / 255.0);
        let name = format!(
            "{cat} #{rgb:06x}{}",
            if t & TAG_TRANSLUCENT != 0 {
                " shell"
            } else {
                ""
            }
        );
        let (m, r, ior) = surface(glass);
        return g.material_pbr(&name, [c(16), c(8), c(0), alpha], m, r, ior, json!({"category": cat}));
    }
    let [r, gr, b] = category_color(cat);
    let name = if glass {
        format!("{cat} glass")
    } else {
        cat.to_string()
    };
    let (m, ro, ior) = surface(glass);
    g.material_pbr(&name, [r, gr, b, alpha], m, ro, ior, json!({"category": cat}))
}

/// Metallic, roughness and refraction index: glass is smooth and refractive, the rest matt.
fn surface(glass: bool) -> (f32, f32, Option<f32>) {
    if glass {
        (0.0, 0.05, Some(1.5))
    } else {
        (0.0, 0.85, None)
    }
}

fn sql_json(v: &Sql) -> Option<Value> {
    Some(match v {
        Sql::Integer(i) => json!(i),
        Sql::Real(f) => json!(f),
        Sql::Text(s) => json!(s),
        Sql::Null | Sql::Blob(_) => return None,
    })
}

/// What a body belongs to.
struct Owner<'a> {
    cat: &'static str,
    at: Option<(&'static str, i64)>,
    level: Option<&'a str>,
}

impl Owner<'_> {
    fn node_name(&self) -> String {
        match self.at {
            Some((_, id)) => format!("{} {id}", self.cat),
            None => self.cat.to_string(),
        }
    }

    /// `{"ddc": {category, level, table, id, props, ifc_guid}}`.
    fn extras(&self, m: &Model) -> Value {
        let mut o = json!({"category": self.cat});
        if let Some(l) = self.level {
            o["level"] = json!(l);
        }
        if let Some((table, id)) = self.at {
            o["table"] = json!(table);
            o["id"] = json!(id);
            let mut props = Props::new();
            for (k, v, unit) in m.props(table, id).unwrap_or_default() {
                if let Some(j) = sql_json(&v) {
                    props.insert(
                        k,
                        if unit.is_empty() {
                            j
                        } else {
                            json!({"value": j, "unit": unit})
                        },
                    );
                }
            }
            if !props.is_empty() {
                o["props"] = Value::Object(props);
            }
            if let Ok(Some((_, guid))) = m.ext_guid(table, id) {
                o["ifc_guid"] = json!(guid);
            }
        }
        json!({ "ddc": o })
    }
}

/// Component identity in node extras: family, type, component category, IFC class, every
/// parameter (`{value, unit, instance}`, SI) and classification codes.
fn comp_extras(o: &mut Value, r: &ddc_model::CompRecord) {
    let mut params = Props::new();
    for p in &r.params {
        let v = match &p.value {
            ddc_model::ParamValue::Num(x) => json!(x),
            ddc_model::ParamValue::Text(t) => json!(t),
        };
        params.insert(
            p.name.clone(),
            json!({ "value": v, "unit": ddc_model::unit_of(p.kind), "instance": p.instance }),
        );
    }
    o["family"] = json!(r.family);
    o["type"] = json!(r.type_name);
    o["comp_category"] = json!(r.category);
    o["ifc_class"] = json!(r.ifc_class);
    o["ifc_predefined"] = json!(r.ifc_predefined);
    o["params"] = Value::Object(params);
    if !r.classification.is_empty() {
        let c: Props = r
            .classification
            .iter()
            .map(|(s, code)| (s.clone(), json!(code)))
            .collect();
        o["classification"] = Value::Object(c);
    }
}

fn add(g: &mut Gltf, m: &Model, o: &Owner, tag: u32, pos: &[[f32; 3]], tris: &[[u32; 3]]) {
    let mat = material(g, o.cat, tag);
    g.add_part(&o.node_name(), mat, pos, tris, o.extras(m));
}

fn lift(positions: &[[f32; 3]], dz: f32) -> Vec<[f32; 3]> {
    positions.iter().map(|p| [p[0], p[1], p[2] + dz]).collect()
}

fn mesh_positions(m: &ddc_geom::mesh::Mesh) -> Vec<[f32; 3]> {
    m.positions
        .iter()
        .map(|p| [p.x as f32, p.y as f32, p.z as f32])
        .collect()
}

/// Model row of a provider's body. Roofs, stairs and railings have no editor selection.
fn part_ref(t: Target) -> Option<(&'static str, i64)> {
    match t.cat {
        Cat::Roof => Some(("roof", t.id)),
        Cat::Stair => Some(("stair", t.id)),
        Cat::Railing => Some(("railing", t.id)),
        // keyed `freeform:<id>` so the exact STEP writer rebuilds the patch
        Cat::Ext(c) if c == crate::freeform_edit3d::CAT => Some(("freeform", t.id)),
        Cat::Ext(EXT_ROOF_EDGE) => crate::roof_edge_scene::elem(t).map(|e| {
            let table = match e.kind() {
                "taper" => "roof_taper",
                k => k,
            };
            (table, t.id)
        }),
        _ => t.sel().and_then(sel_ref),
    }
}

fn part_category(t: Target, at: Option<(&'static str, i64)>) -> &'static str {
    match t.cat {
        Cat::Ext(_) => at.map_or("other", |a| a.0),
        c => c.key(),
    }
}

/// `SELECT id, level` as a map; `NULL` level stays `None`.
fn levels_of(m: &Model, sql: &str) -> Result<HashMap<i64, Option<i64>>, rusqlite::Error> {
    let mut st = m.sql().prepare(sql)?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;
    rows.collect()
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// The whole model as glTF. Walks the levels on `ed` (as `scene_all` does) and puts level,
/// selection and revision back, also when a level fails.
pub fn build(ed: &mut Editor, name: &str) -> Result<Export, String> {
    let (level, sel, rev) = (ed.level, ed.selection.clone(), ed.revision);
    let (under, providers) = (ed.underlay_on, ed.scene_providers);
    ed.underlay_on = false;
    ed.scene_providers = true;
    let out = build_all(ed, name);
    ed.level = level;
    ed.underlay_on = under;
    ed.scene_providers = providers;
    let back = ed.reload();
    ed.selection = sel;
    ed.restore_revision(rev);
    let out = out?;
    back.map_err(err)?;
    Ok(out)
}

fn build_all(ed: &mut Editor, name: &str) -> Result<Export, String> {
    let mut g = Gltf::new(name);
    let mut issues = vec![];
    let levels = ed.model.level_list().map_err(err)?;
    let names: HashMap<i64, &str> = levels.iter().map(|l| (l.0, l.1.as_str())).collect();
    // a mesh without a level comes from every level's provider call: once is enough
    let loose: HashSet<i64> = levels_of(&ed.model, "SELECT id, level FROM mesh")
        .map_err(err)?
        .into_iter()
        .filter(|x| x.1.is_none())
        .map(|x| x.0)
        .collect();

    for (i, (id, lname, _)) in levels.iter().enumerate() {
        ed.level = *id;
        ed.reload().map_err(err)?;
        let dz = ed.elevation as f32;
        let (scene, problems) = ed.scene_report();
        issues.extend(problems);
        for m in &scene.meshes {
            let m: &SceneMesh = m;
            // solids of all levels come with every level's report: added once below
            if (SOLID_BASE..SOLID_BASE + SOLID_SPAN).contains(&m.element) {
                continue;
            }
            let sel = ed.sel_of_element(m.element);
            // the window body is a thin pane in the wall's hole; curtain frames share its tag
            let tag = if matches!(sel, Some(Sel::Opening(_))) && base_tag(m.tag) == TAG_WINDOW {
                TAG_GLASS
            } else {
                m.tag
            };
            let o = Owner {
                cat: mesh_category(ed, m),
                at: sel.and_then(sel_ref),
                level: Some(lname.as_str()),
            };
            add(
                &mut g,
                &ed.model,
                &o,
                tag,
                &lift(&m.positions, dz),
                &m.triangles,
            );
        }
        for (p, _) in scene3d::parts(ed, *id, true, false) {
            let is_loose = p.target.cat == Cat::Mesh && loose.contains(&p.target.id);
            if p.target.cat == Cat::Ext(EXT_SOLID) || (is_loose && i > 0) {
                continue;
            }
            let at = part_ref(p.target);
            let o = Owner {
                cat: part_category(p.target, at),
                at,
                level: (!is_loose).then_some(lname.as_str()),
            };
            let pos: Vec<[f32; 3]> = p.pos.iter().map(|v| [v.x, v.y, v.z + dz]).collect();
            add(&mut g, &ed.model, &o, p.tag, &pos, &p.tris);
        }
    }
    let ed: &Editor = ed;
    let m = &ed.model;

    let (solids, bad) = crate::solid_scene::parts(m, None, 0.0);
    issues.extend(bad.into_iter().map(|(id, e)| format!("solid {id}: {e}")));
    for p in solids.iter().filter(|p| !p.void) {
        let o = Owner {
            cat: "solid",
            at: Some(("solid", p.id)),
            level: m
                .solid(p.id)
                .ok()
                .and_then(|s| names.get(&s.level).copied()),
        };
        add(&mut g, m, &o, p.tag, &p.pos, &p.tris);
    }

    let mut bodies = vec![];
    crate::mass_tools::scene_meshes(m, 0.0, &mut bodies);
    for b in &bodies {
        let b: &SceneMesh = b;
        let o = Owner {
            cat: "mass",
            at: ed.sel_of_element(b.element).and_then(sel_ref),
            level: None,
        };
        add(&mut g, m, &o, b.tag, &b.positions, &b.triangles);
    }

    let topo = m.toposolids().map_err(err)?;
    let mut bodies = vec![];
    crate::topo::append_scene(ed, &mut bodies);
    let dz = ed.elevation as f32;
    for b in &bodies {
        let b: &SceneMesh = b;
        let row = b
            .element
            .checked_sub(TOPO_BASE)
            .and_then(|k| topo.get(k as usize));
        let o = Owner {
            cat: "topo",
            at: row.map(|r| ("toposolid", r.0)),
            level: None,
        };
        add(&mut g, m, &o, b.tag, &lift(&b.positions, dz), &b.triangles);
    }

    let info = m.mep_run_info().map_err(err)?;
    for b in m.mep_bodies(None).map_err(err)? {
        let Some(ri) = info.get(&b.run) else { continue };
        let shell = matches!(b.part, MepPart::Insulation | MepPart::Lining);
        let o = Owner {
            cat: "mep",
            at: Some(("mep_run", b.run)),
            level: names.get(&ri.level).copied(),
        };
        add(
            &mut g,
            m,
            &o,
            tag_rgb(ri.color, shell),
            &mesh_positions(&b.mesh),
            &b.mesh.triangles,
        );
    }

    let comp_levels = levels_of(m, "SELECT id, level FROM comp").map_err(err)?;
    let cs = m.component_scene(Lod::Fine, None).map_err(err)?;
    let recs: HashMap<i64, ddc_model::CompRecord> = m
        .component_records(None)
        .map_err(err)?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let cat = crate::view3d::sel_category(ed, Sel::Comp(0));
    let mut by: Vec<Vec<([f32; 16], Value)>> = vec![vec![]; cs.meshes.len()];
    for i in &cs.instances {
        let r = i.matrix();
        let f = |k: usize| r[k] as f32;
        let place = [
            f(0),
            f(4),
            f(8),
            0.0,
            f(1),
            f(5),
            f(9),
            0.0,
            f(2),
            f(6),
            f(10),
            0.0,
            f(3),
            f(7),
            f(11),
            1.0,
        ];
        let o = Owner {
            cat,
            at: Some(("comp", i.id)),
            level: comp_levels
                .get(&i.id)
                .copied()
                .flatten()
                .and_then(|l| names.get(&l).copied()),
        };
        if let Some(v) = by.get_mut(i.mesh) {
            let mut x = o.extras(m);
            if let Some(r) = recs.get(&i.id) {
                comp_extras(&mut x["ddc"], r);
            }
            v.push((place, x));
        }
    }
    for (k, places) in by.iter().enumerate().filter(|(_, v)| !v.is_empty()) {
        let mat = material(&mut g, cat, 0);
        g.add_instances(
            &format!("{cat} x{}", places.len()),
            mat,
            &mesh_positions(&cs.meshes[k]),
            &cs.meshes[k].triangles,
            places,
        );
    }

    issues.sort();
    issues.dedup();
    Ok(Export { gltf: g, issues })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::{Event, Mode};
    use ddc_geom::v2;
    use ddc_model::{CompHost, CompSpec, WallSpec};
    use ddc_render::scene::TAG_MULLION;

    fn node<'a>(g: &'a Gltf, table: &str, id: i64) -> &'a ddc_io::gltf::Node {
        g.nodes
            .iter()
            .find(|n| n.extras["ddc"]["table"] == table && n.extras["ddc"]["id"] == id)
            .unwrap()
    }

    /// The JSON chunk of a .glb, as a reader sees it.
    fn glb_json(bytes: &[u8]) -> Value {
        assert_eq!(&bytes[0..4], b"glTF");
        let n = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        assert_eq!(&bytes[16..20], b"JSON");
        serde_json::from_slice(&bytes[20..20 + n]).unwrap()
    }

    #[test]
    fn components_carry_family_type_class_and_params_and_share_meshes() {
        let mut ed = Editor::new_in_memory().unwrap();
        let fam = ed.model.load_library_family("Table").unwrap();
        let ctype = ed.model.comp_types(fam).unwrap()[0].0;
        let level = ed.level;
        let mut ids = vec![];
        for x in [0.0, 3.0] {
            ids.push(
                ed.model
                    .add_component(&CompSpec::at(level, ctype, x, 0.0))
                    .unwrap(),
            );
        }
        let pid = ed
            .model
            .add_param(&ddc_model::ParamSpec {
                name: format!("{}OmniClass", ddc_model::CLASSIFICATION_PREFIX),
                ptype: ddc_model::ParamType::Text,
                per_type: false,
                unit: String::new(),
                group: String::new(),
                guid: None,
                categories: vec!["comp".into()],
            })
            .unwrap();
        ed.model
            .set_param_values(&[ddc_model::ParamSet {
                param: pid,
                kind: "comp".into(),
                id: ids[0],
                value: Some(ddc_model::PVal::Text("23-21 11 13".into())),
            }])
            .unwrap();
        ed.reload().unwrap();
        let g = build(&mut ed, "t").unwrap().gltf;
        let back = glb_json(&ddc_io::gltf::write_glb(&g).unwrap());
        let nodes: Vec<&Value> = back["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|n| n["extras"]["ddc"]["table"] == "comp")
            .collect();
        assert_eq!(nodes.len(), 2);
        // one mesh, two placements
        assert_eq!(nodes[0]["mesh"], nodes[1]["mesh"]);
        let d = &nodes
            .iter()
            .find(|n| n["extras"]["ddc"]["id"] == ids[0])
            .unwrap()["extras"]["ddc"];
        assert_eq!(d["family"], "Table");
        assert_eq!(d["type"], ed.model.comp_types(fam).unwrap()[0].1);
        assert_eq!((&d["ifc_class"], &d["ifc_predefined"]), (&json!("IfcFurniture"), &json!("TABLE")));
        assert_eq!(d["comp_category"], "furniture");
        let w = &d["params"]["Width"];
        assert!((w["value"].as_f64().unwrap() - 1.6).abs() < 1e-12);
        assert_eq!((&w["unit"], &w["instance"]), (&json!("m"), &json!(false)));
        assert_eq!(d["params"]["Wood"]["value"], "Oak");
        assert_eq!(d["params"]["Seats"]["value"], 4.0);
        assert_eq!(d["classification"], json!({ "OmniClass": "23-21 11 13" }));
        let other = &nodes
            .iter()
            .find(|n| n["extras"]["ddc"]["id"] == ids[1])
            .unwrap()["extras"]["ddc"];
        assert!(other.get("classification").is_none());
    }

    #[test]
    fn walls_export_with_ids_and_categories() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Release(v2(4.0, 3.0)));
        ed.set_mode(Mode::Select);
        let g = build(&mut ed, "t").unwrap().gltf;
        assert!(!g.nodes.is_empty());
        let first = &g.nodes[0].extras["ddc"];
        assert_eq!(first["category"], "wall");
        assert_eq!(first["table"], "wall");
        assert!(g.materials.iter().any(|m| m.name == "wall"));
        let bytes = ddc_io::gltf::write_glb(&g).unwrap();
        assert_eq!(&bytes[0..4], b"glTF");
    }

    #[test]
    fn an_empty_model_has_no_nodes() {
        let mut ed = Editor::new_in_memory().unwrap();
        assert!(build(&mut ed, "t").unwrap().gltf.nodes.is_empty());
    }

    #[test]
    fn inactive_level_comes_with_ids_heights_and_categories() {
        let mut ed = Editor::new_in_memory().unwrap();
        let t = ed.types[1].id;
        let up = ed.model.add_level("L2", 3.0).unwrap();
        let wall = ed
            .model
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (4.0, 0.0),
                level: up,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let fam = ed
            .model
            .load_family(ddc_model::library_sources()[0])
            .unwrap();
        let ctype = ed.model.comp_types(fam).unwrap()[0].0;
        let comp = ed
            .model
            .add_component(&CompSpec {
                level: up,
                ctype,
                at: (1.0, 2.0, 0.0),
                rotation: 0.0,
                mirror: false,
                tilt: None,
                host: CompHost::None,
                values: vec![],
            })
            .unwrap();
        ed.reload().unwrap();
        let (level, rev, elev) = (ed.level, ed.revision, ed.elevation);
        assert_ne!(level, up);

        let g = build(&mut ed, "t").unwrap().gltf;

        let w = node(&g, "wall", wall);
        assert_eq!(w.extras["ddc"]["category"], "wall");
        assert_eq!(w.extras["ddc"]["level"], "L2");
        assert!(
            (w.translation[2] - 3.0).abs() < 1e-3,
            "{}",
            w.translation[2]
        );

        let c = node(&g, "comp", comp);
        assert_eq!(c.extras["ddc"]["level"], "L2");
        let place = c.matrix.unwrap();
        assert!((place[12] - 1.0).abs() < 1e-3 && (place[14] - 3.0).abs() < 1e-3);
        // furniture is not MEP
        assert_eq!(g.materials[g.meshes[c.mesh].material].name, "other");

        assert_eq!((ed.level, ed.revision, ed.elevation), (level, rev, elev));
    }

    #[test]
    fn colours_are_linear_and_only_glass_is_see_through() {
        let mut g = Gltf::new("t");
        let frame = material(&mut g, "curtain", TAG_MULLION);
        let pane = material(&mut g, "curtain", TAG_GLASS);
        assert_eq!(g.materials[frame].color[3], 1.0);
        assert_eq!(g.materials[pane].color[3], GLASS_ALPHA);
        let run = material(&mut g, "mep", tag_rgb([128, 128, 128], false));
        let c = g.materials[run].color;
        assert!((c[0] - 0.2158).abs() < 1e-3 && c[3] == 1.0, "{c:?}");
        let shell = material(&mut g, "mep", tag_rgb([128, 128, 128], true));
        assert_eq!(g.materials[shell].color[3], GLASS_ALPHA);
    }

    #[test]
    fn glass_is_smooth_and_refractive_and_the_rest_matt() {
        let mut g = Gltf::new("t");
        let glass = material(&mut g, "window", TAG_GLASS);
        let wall = material(&mut g, "wall", 0);
        assert_eq!(g.materials[glass].ior, Some(1.5));
        assert!(g.materials[glass].roughness < 0.2);
        assert_eq!(g.materials[wall].ior, None);
        assert!(g.materials[wall].roughness > 0.5);
    }
}
