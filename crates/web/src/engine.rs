// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Platform-free part of the browser viewer: open a project from bytes, tessellate it with the
//! model and geometry crates, pick, read properties and run the model checks.

use std::collections::HashMap;

use ddc_model::checks::{self, CheckScope, Severity};
use ddc_model::Model;
use rusqlite::types::Value;

/// One drawable element: a contiguous run of `Scene::pos` (three vertices per triangle).
#[derive(Clone, Debug)]
pub struct Item {
    pub table: String,
    pub id: i64,
    pub name: String,
    pub cat: String,
    pub level: Option<i64>,
    pub first: u32,
    pub count: u32,
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

#[derive(Default)]
pub struct Scene {
    pub items: Vec<Item>,
    pub pos: Vec<[f32; 3]>,
    pub nrm: Vec<[f32; 3]>,
    pub col: Vec<[u8; 4]>,
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

pub struct Project {
    pub name: String,
    pub kind: &'static str,
    pub model: Model,
    pub levels: Vec<(i64, String, f64)>,
    pub warnings: Vec<String>,
}

/// A `.ddc` project or an IFC file, by file name.
pub fn open(name: &str, bytes: &[u8]) -> Result<Project, String> {
    ddc_geom::layered::set_threads(1);
    let lower = name.to_ascii_lowercase();
    let mut warnings = vec![];
    let (model, kind) = if bytes.starts_with(b"SQLite format 3\0") {
        (Model::from_bytes(bytes).map_err(|e| e.to_string())?, "ddc")
    } else if lower.ends_with(".ifc") || bytes.starts_with(b"ISO-10303-21") {
        let mut m = Model::open(":memory:").map_err(|e| e.to_string())?;
        let rep = ddc_io::ifc::import_ifc(&mut m, bytes).map_err(|e| e.to_string())?;
        warnings.extend(rep.warnings);
        (m, "ifc")
    } else {
        return Err(format!("{name}: not a .ddc project or an IFC file"));
    };
    let levels = model.level_list().map_err(|e| e.to_string())?;
    Ok(Project {
        name: name.to_string(),
        kind,
        model,
        levels,
        warnings,
    })
}

fn srgb(c: [f32; 4]) -> [u8; 4] {
    use ddc_io::meshx::linear_to_srgb8 as s;
    [s(c[0]), s(c[1]), s(c[2]), (c[3].clamp(0.0, 1.0) * 255.0) as u8]
}

struct Raw {
    table: String,
    id: i64,
    name: String,
    cat: String,
    level: Option<i64>,
    rgba: [u8; 4],
    pos: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
}

fn level_of(m: &Model, table: &str, id: i64) -> Option<i64> {
    m.sql()
        .query_row(&format!("SELECT level FROM {table} WHERE id=?1"), [id], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .ok()
        .flatten()
}

fn ids(m: &Model, table: &str) -> Vec<i64> {
    let Ok(mut st) = m.sql().prepare(&format!("SELECT id FROM {table} ORDER BY id")) else {
        return vec![];
    };
    st.query_map([], |r| r.get(0))
        .map(|r| r.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn push_meshes(out: &mut Vec<Raw>, m: &Model, table: &str, cat: &str, id: i64, ms: &[ddc_geom::mesh::Mesh]) {
    let mut pos = vec![];
    let mut tris = vec![];
    for mesh in ms {
        let base = pos.len() as u32;
        pos.extend(mesh.positions.iter().map(|v| [v.x, v.y, v.z]));
        tris.extend(mesh.triangles.iter().map(|t| t.map(|k| k + base)));
    }
    if tris.is_empty() {
        return;
    }
    out.push(Raw {
        table: table.into(),
        id,
        name: format!("{cat} {id}"),
        cat: cat.into(),
        level: level_of(m, table, id),
        rgba: srgb(ddc_io::meshx::category_rgba(cat)),
        pos,
        tris,
    });
}

/// Every element body the engine tessellates without the editor: walls, slabs, roofs, columns,
/// beams, curtain walls, masses, ceilings, MEP runs, imported meshes, stairs, railings and
/// library components (doors, windows, furniture, fixtures).
pub fn tessellate(m: &Model) -> (Scene, Vec<String>) {
    let mut raw: Vec<Raw> = vec![];
    let mut warns = vec![];
    match ddc_io::meshx::MeshSet::from_model(m) {
        Ok((set, w)) => {
            // the stair/railing/component note is about this exporter; they are added below
            warns.extend(w.into_iter().filter(|s| !s.contains("tessellated only by the editor")));
            for p in set.parts {
                let (table, id) = match p.key.split_once(':') {
                    Some((t, i)) => (t.to_string(), i.parse().unwrap_or(0)),
                    None => (p.key.clone(), 0),
                };
                let rgba = p
                    .material
                    .and_then(|k| set.materials.get(k))
                    .map_or([200, 200, 200, 255], |mt| srgb(mt.rgba));
                let level = level_of(m, &table, id);
                raw.push(Raw {
                    table,
                    id,
                    name: p.name,
                    cat: p.category,
                    level,
                    rgba,
                    pos: p.positions,
                    tris: p.triangles,
                });
            }
        }
        Err(e) => warns.push(e),
    }
    for id in ids(m, "stair") {
        if let Ok(ms) = m.stair_meshes(id) {
            push_meshes(&mut raw, m, "stair", "stair", id, &ms);
        }
    }
    for id in ids(m, "railing") {
        if let Ok(ms) = m.railing_meshes(id) {
            push_meshes(&mut raw, m, "railing", "railing", id, &ms);
        }
    }
    match m.component_scene(ddc_geom::component::Lod::Coarse, None) {
        Ok(cs) => {
            let fam: HashMap<i64, String> = m.families().unwrap_or_default().into_iter().collect();
            for inst in &cs.instances {
                let mesh = &cs.meshes[inst.mesh];
                let pos = mesh
                    .positions
                    .iter()
                    .map(|v| {
                        let (x, y, z) = inst.placement.apply((v.x, v.y, v.z));
                        [x, y, z]
                    })
                    .collect();
                let fname = fam.get(&inst.family).cloned().unwrap_or_else(|| "component".into());
                let rgba = comp_rgba(&fname);
                raw.push(Raw {
                    table: "comp".into(),
                    id: inst.id,
                    name: format!("{fname} {}", inst.id),
                    cat: fname,
                    level: level_of(m, "comp", inst.id),
                    rgba,
                    pos,
                    tris: mesh.triangles.clone(),
                });
            }
        }
        Err(e) => warns.push(format!("components: {e}")),
    }
    // grouped by level so a level can be hidden with one draw range
    raw.sort_by_key(|r| (r.level.unwrap_or(i64::MAX), r.table.clone(), r.id));
    (flatten(raw), warns)
}

fn comp_rgba(family: &str) -> [u8; 4] {
    let f = family.to_ascii_lowercase();
    if f.contains("window") || f.contains("glaz") {
        [150, 190, 220, 255]
    } else if f.contains("door") {
        [176, 132, 92, 255]
    } else {
        [196, 188, 172, 255]
    }
}

fn flatten(raw: Vec<Raw>) -> Scene {
    let mut s = Scene {
        lo: [f32::MAX; 3],
        hi: [f32::MIN; 3],
        ..Default::default()
    };
    for r in raw {
        let first = s.pos.len() as u32;
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for t in &r.tris {
            let Some(p) = t
                .iter()
                .map(|&k| r.pos.get(k as usize).map(|v| v.map(|c| c as f32)))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let n = normal(p[0], p[1], p[2]);
            for v in p {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
                s.pos.push(v);
                s.nrm.push(n);
                s.col.push(r.rgba);
            }
        }
        let count = s.pos.len() as u32 - first;
        if count == 0 {
            continue;
        }
        for k in 0..3 {
            s.lo[k] = s.lo[k].min(lo[k]);
            s.hi[k] = s.hi[k].max(hi[k]);
        }
        s.items.push(Item {
            table: r.table,
            id: r.id,
            name: r.name,
            cat: r.cat,
            level: r.level,
            first,
            count,
            lo,
            hi,
        });
    }
    if s.items.is_empty() {
        s.lo = [0.0; 3];
        s.hi = [1.0; 3];
    }
    s
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let n = cross(sub(b, a), sub(c, a));
    let l = dot(n, n).sqrt();
    if l > 0.0 {
        n.map(|v| v / l)
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn ray_box(o: [f32; 3], d: [f32; 3], lo: [f32; 3], hi: [f32; 3]) -> Option<f32> {
    let (mut t0, mut t1) = (0.0f32, f32::MAX);
    for k in 0..3 {
        let inv = 1.0 / d[k];
        let (mut a, mut b) = ((lo[k] - o[k]) * inv, (hi[k] - o[k]) * inv);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return None;
        }
    }
    Some(t0)
}

/// Möller–Trumbore, both faces.
fn ray_tri(o: [f32; 3], d: [f32; 3], a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> Option<f32> {
    let e1 = sub(b, a);
    let e2 = sub(c, a);
    let p = cross(d, e2);
    let det = dot(e1, p);
    if det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    let s = sub(o, a);
    let u = dot(s, p) * inv;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = cross(s, e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    let t = dot(e2, q) * inv;
    (t > 0.0).then_some(t)
}

impl Scene {
    /// Nearest item hit by the ray among those `visible` accepts.
    pub fn pick(&self, o: [f32; 3], d: [f32; 3], visible: impl Fn(&Item) -> bool) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (i, it) in self.items.iter().enumerate() {
            if !visible(it) {
                continue;
            }
            let Some(tb) = ray_box(o, d, it.lo, it.hi) else {
                continue;
            };
            if best.is_some_and(|(t, _)| tb > t) {
                continue;
            }
            let (f, n) = (it.first as usize, it.count as usize);
            for tri in self.pos[f..f + n].chunks_exact(3) {
                if let Some(t) = ray_tri(o, d, tri[0], tri[1], tri[2]) {
                    if best.is_none_or(|(bt, _)| t < bt) {
                        best = Some((t, i));
                    }
                }
            }
        }
        best.map(|b| b.1)
    }

    pub fn find(&self, table: &str, id: i64) -> Option<usize> {
        self.items.iter().position(|it| it.table == table && it.id == id)
    }

    pub fn triangles(&self) -> usize {
        self.pos.len() / 3
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::Integer(i) => Some(i.to_string()),
        Value::Real(f) => Some(format!("{}", (f * 1e6).round() / 1e6)),
        Value::Text(s) if s.chars().count() > 300 => Some(format!("{}…", s.chars().take(300).collect::<String>())),
        Value::Text(s) => Some(s.clone()),
        Value::Blob(b) => Some(format!("<{} bytes>", b.len())),
    }
}

/// Properties of an element: its stored row, then imported property sets.
pub fn properties(p: &Project, table: &str, id: i64) -> Vec<(String, String)> {
    let mut out = vec![];
    let db = p.model.sql();
    let known = [
        "wall", "slab", "roof", "col", "beam", "curtain_wall", "mass", "ceiling", "mep_run", "mesh",
        "stair", "railing", "comp", "room", "opening",
    ];
    if !known.contains(&table) {
        return out;
    }
    if let Ok(mut st) = db.prepare(&format!("SELECT * FROM {table} WHERE id=?1")) {
        let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        let row = st.query_row([id], |r| {
            (0..names.len()).map(|k| r.get::<_, Value>(k)).collect::<Result<Vec<_>, _>>()
        });
        if let Ok(vals) = row {
            for (n, v) in names.iter().zip(&vals) {
                if matches!(v, Value::Blob(_)) {
                    continue;
                }
                if let Some(t) = text(v) {
                    let t = match (n.as_str(), v) {
                        ("level", Value::Integer(l)) => p
                            .levels
                            .iter()
                            .find(|x| x.0 == *l)
                            .map_or(t.clone(), |x| format!("{} ({t})", x.1)),
                        _ => t,
                    };
                    out.push((n.clone(), t));
                }
            }
        }
    }
    if table == "comp" {
        if let Ok(q) = p.model.component_quantities(id) {
            out.push(("volume, m³".into(), format!("{:.3}", q.volume)));
        }
    }
    for (k, v, unit) in p.model.props(table, id).unwrap_or_default() {
        if let Some(t) = text(&v) {
            let t = if unit.is_empty() { t } else { format!("{t} {unit}") };
            out.push((k, t));
        }
    }
    out
}

pub struct AuditRow {
    pub severity: Severity,
    pub rule: &'static str,
    pub text: String,
    pub elements: Vec<(String, i64)>,
}

const CATALOG: &str = include_str!(concat!(env!("OUT_DIR"), "/checks_en.json"));

fn catalog() -> &'static HashMap<String, String> {
    static C: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
    C.get_or_init(|| serde_json::from_str(CATALOG).unwrap_or_default())
}

/// Every built-in and project rule over the whole model, errors first.
pub fn audit(p: &Project, limit: usize) -> Result<Vec<AuditRow>, String> {
    let found = checks::run_all(&p.model, None, &CheckScope::All, limit).map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .map(|f| {
            let mut s = catalog().get(f.msg_key).cloned().unwrap_or_else(|| f.msg_key.to_string());
            for (k, v) in &f.args {
                s = s.replace(&format!("{{{k}}}"), v);
            }
            AuditRow {
                severity: f.severity,
                rule: f.rule,
                text: s,
                elements: f.elements,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set DDC_WEB_SAMPLE to a .ddc file to measure a real project.
    #[test]
    fn sample_project_opens_and_tessellates() {
        let Ok(path) = std::env::var("DDC_WEB_SAMPLE") else {
            return;
        };
        let bytes = std::fs::read(&path).unwrap();
        let t0 = web_time::Instant::now();
        let p = open(&path, &bytes).unwrap();
        let (s, w) = tessellate(&p.model);
        let a = audit(&p, 500).unwrap();
        let mut cats: HashMap<&str, usize> = HashMap::new();
        for it in &s.items {
            *cats.entry(it.table.as_str()).or_default() += 1;
        }
        eprintln!(
            "items={} tris={} audit={} warns={w:?} {:?} {:?}",
            s.items.len(),
            s.triangles(),
            a.len(),
            cats,
            t0.elapsed()
        );
        assert!(!s.items.is_empty());
        let it = &s.items[0];
        assert!(!properties(&p, &it.table, it.id).is_empty());
    }

    #[test]
    fn ifc_and_garbage() {
        assert!(open("x.txt", b"hello").is_err());
    }
}
