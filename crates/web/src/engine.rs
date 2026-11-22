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
    /// Building system the element serves (HVAC, water, drainage, electrical, fire).
    pub group: Option<&'static str>,
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
    [
        s(c[0]),
        s(c[1]),
        s(c[2]),
        (c[3].clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

struct Raw {
    table: String,
    id: i64,
    name: String,
    cat: String,
    level: Option<i64>,
    rgba: [u8; 4],
    /// Colour of each triangle when the parts differ (empty: `rgba` for all).
    tri_rgba: Vec<[u8; 4]>,
    group: Option<&'static str>,
    pos: Vec<[f64; 3]>,
    tris: Vec<[u32; 3]>,
}

fn level_of(m: &Model, table: &str, id: i64) -> Option<i64> {
    m.sql()
        .query_row(
            &format!("SELECT level FROM {table} WHERE id=?1"),
            [id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .ok()
        .flatten()
}

fn ids(m: &Model, table: &str) -> Vec<i64> {
    let Ok(mut st) = m
        .sql()
        .prepare(&format!("SELECT id FROM {table} ORDER BY id"))
    else {
        return vec![];
    };
    st.query_map([], |r| r.get(0))
        .map(|r| r.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn push_meshes(
    out: &mut Vec<Raw>,
    m: &Model,
    table: &str,
    cat: &str,
    id: i64,
    ms: &[ddc_geom::mesh::Mesh],
) {
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
        tri_rgba: vec![],
        group: None,
        pos,
        tris,
    });
}

/// Tessellation in steps, so a page can show progress between them.
pub struct Tess {
    raw: Vec<Raw>,
    warns: Vec<String>,
    next: usize,
}

impl Tess {
    pub const STEPS: [&'static str; 4] = [
        "walls, floors, roofs and MEP",
        "stairs and railings",
        "components",
        "doors, windows and site",
    ];

    pub fn new() -> Self {
        Tess {
            raw: vec![],
            warns: vec![],
            next: 0,
        }
    }

    /// Index of the step `step` runs next.
    pub fn next(&self) -> usize {
        self.next
    }

    /// Runs the next step; the scene once the last one is done.
    pub fn step(&mut self, m: &Model) -> Option<(Scene, Vec<String>)> {
        let (raw, warns) = (&mut self.raw, &mut self.warns);
        self.next += 1;
        match self.next - 1 {
            0 => {
                match ddc_io::meshx::MeshSet::from_model(m) {
                    Ok((set, w)) => {
                        // the stair/railing/component note is about this exporter; they are added below
                        warns.extend(
                            w.into_iter()
                                .filter(|s| !s.contains("tessellated only by the editor")),
                        );
                        let runs = mep_runs(m);
                        for p in set.parts {
                            let (table, id) = match p.key.split_once(':') {
                                Some((t, i)) => (t.to_string(), i.parse().unwrap_or(0)),
                                None => (p.key.clone(), 0),
                            };
                            let mut rgba = p
                                .material
                                .and_then(|k| set.materials.get(k))
                                .map_or([200, 200, 200, 255], |mt| srgb(mt.rgba));
                            let (mut name, mut cat, mut group) = (p.name, p.category, None);
                            if let Some((domain, kind)) =
                                runs.get(&id).filter(|_| table == "mep_run")
                            {
                                let (g, c) = system_style(kind.as_deref());
                                rgba = c;
                                group = g;
                                cat = cap(domain);
                                name = format!(
                                    "{} {id}",
                                    kind.as_deref().unwrap_or(domain).replace('_', " ")
                                );
                            }
                            let level = level_of(m, &table, id);
                            raw.push(Raw {
                                table,
                                id,
                                name,
                                cat,
                                level,
                                rgba,
                                tri_rgba: vec![],
                                group,
                                pos: p.positions,
                                tris: p.triangles,
                            });
                        }
                    }
                    Err(e) => warns.push(e),
                }
            }
            1 => {
                for id in ids(m, "stair") {
                    if let Ok(ms) = m.stair_meshes(id) {
                        push_meshes(raw, m, "stair", "stair", id, &ms);
                    }
                }
                for id in ids(m, "railing") {
                    if let Ok(ms) = m.railing_meshes(id) {
                        push_meshes(raw, m, "railing", "railing", id, &ms);
                    }
                }
            }
            2 => {
                match m.component_scene(ddc_geom::component::Lod::Coarse, None) {
                    Ok(cs) => {
                        let fam: HashMap<i64, String> =
                            m.families().unwrap_or_default().into_iter().collect();
                        let mut mats: HashMap<String, Option<[u8; 4]>> = HashMap::new();
                        // per mesh, not per instance: thousands of placements share a few dozen bodies
                        let tri_cols: Vec<Vec<Option<[u8; 4]>>> = cs
                            .meshes
                            .iter()
                            .zip(&cs.materials)
                            .map(|(mesh, parts)| {
                                mesh.face_of
                                    .iter()
                                    .map(|&f| {
                                        let name = parts.get(f as usize)?;
                                        *mats
                                            .entry(name.clone())
                                            .or_insert_with(|| material_rgba(m, name))
                                    })
                                    .collect()
                            })
                            .collect();
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
                            let fname = fam
                                .get(&inst.family)
                                .cloned()
                                .unwrap_or_else(|| "component".into());
                            let rgba = comp_rgba(&fname);
                            let cols = tri_cols.get(inst.mesh).map_or(&[][..], |c| &c[..]);
                            let tri_rgba = if cols.len() == mesh.triangles.len()
                                && cols.iter().any(Option::is_some)
                            {
                                cols.iter().map(|c| c.unwrap_or(rgba)).collect()
                            } else {
                                vec![]
                            };
                            raw.push(Raw {
                                table: "comp".into(),
                                id: inst.id,
                                name: format!("{fname} {}", inst.id),
                                group: family_group(&fname),
                                cat: fname,
                                level: level_of(m, "comp", inst.id),
                                rgba,
                                tri_rgba,
                                pos,
                                tris: mesh.triangles.clone(),
                            });
                        }
                    }
                    Err(e) => warns.push(format!("components: {e}")),
                }
            }
            3 => {
                openings(m, raw);
                ground(m, raw);
            }
            _ => {}
        }
        (self.next == Self::STEPS.len()).then(|| {
            let mut raw = std::mem::take(&mut self.raw);
            // grouped by level so a level can be hidden with one draw range
            raw.sort_by_key(|r| (r.level.unwrap_or(i64::MAX), r.table.clone(), r.id));
            (flatten(raw), std::mem::take(&mut self.warns))
        })
    }
}

impl Default for Tess {
    fn default() -> Self {
        Self::new()
    }
}

/// Every element body the engine tessellates without the editor: walls, slabs, roofs, columns,
/// beams, curtain walls, masses, ceilings, MEP runs, imported meshes, stairs, railings and
/// library components (doors, windows, furniture, fixtures).
pub fn tessellate(m: &Model) -> (Scene, Vec<String>) {
    let mut t = Tess::new();
    loop {
        if let Some(r) = t.step(m) {
            return r;
        }
    }
}

/// Doors and windows as panels through their host wall. The editor cuts the wall and builds the
/// frame and leaf; this viewer shows where they are, so they can be seen and picked.
fn openings(m: &Model, raw: &mut Vec<Raw>) {
    let walls: HashMap<i64, usize> = raw
        .iter()
        .enumerate()
        .filter(|(_, r)| r.table == "wall")
        .map(|(i, r)| (r.id, i))
        .collect();
    let sql = "SELECT o.id, o.wall, o.along, o.width, o.sill, o.height, o.kind, w.ax, w.ay, w.bx, w.by, w.bulge \
               FROM opening o JOIN wall w ON w.id=o.wall ORDER BY o.id";
    type Row = (i64, i64, f64, f64, f64, f64, String, [f64; 4], f64);
    let rows: Vec<Row> = m
        .sql()
        .prepare(sql)
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    [r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?],
                    r.get(11)?,
                ))
            })?
            .collect()
        })
        .unwrap_or_default();
    for (id, wall, along, width, sill, height, kind, [ax, ay, bx, by], bulge) in rows {
        let Some(&wi) = walls.get(&wall) else {
            continue;
        };
        let len = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
        if kind == "void" || bulge != 0.0 || len < 1e-6 || width <= 0.0 || height <= 0.0 {
            continue;
        }
        let u = [(bx - ax) / len, (by - ay) / len];
        let n = [-u[1], u[0]];
        let w = &raw[wi];
        // the wall body's faces across its axis and its base, from its own mesh
        let (mut lo, mut hi, mut z0) = (f64::MAX, f64::MIN, f64::MAX);
        for p in &w.pos {
            let d = (p[0] - ax) * n[0] + (p[1] - ay) * n[1];
            lo = lo.min(d);
            hi = hi.max(d);
            z0 = z0.min(p[2]);
        }
        if lo > hi {
            continue;
        }
        let (lo, hi) = (lo - 0.01, hi + 0.01);
        let at =
            |s: f64, d: f64, z: f64| [ax + u[0] * s + n[0] * d, ay + u[1] * s + n[1] * d, z0 + z];
        let (s0, s1, z1, z2) = (along, along + width, sill, sill + height);
        let pos = vec![
            at(s0, lo, z1),
            at(s1, lo, z1),
            at(s1, hi, z1),
            at(s0, hi, z1),
            at(s0, lo, z2),
            at(s1, lo, z2),
            at(s1, hi, z2),
            at(s0, hi, z2),
        ];
        let tris = vec![
            [0, 1, 5],
            [0, 5, 4],
            [1, 2, 6],
            [1, 6, 5],
            [2, 3, 7],
            [2, 7, 6],
            [3, 0, 4],
            [3, 4, 7],
            [4, 5, 6],
            [4, 6, 7],
            [0, 3, 2],
            [0, 2, 1],
        ];
        let door = kind == "door";
        raw.push(Raw {
            table: "opening".into(),
            id,
            name: format!("{kind} {id}"),
            cat: if door { "door".into() } else { "window".into() },
            level: w.level,
            rgba: if door {
                [150, 108, 72, 255]
            } else {
                [120, 170, 210, 255]
            },
            tri_rgba: vec![],
            group: None,
            pos,
            tris,
        });
    }
}

/// Table name of the ground plane item: drawn and hidden like the rest, never picked.
pub const GROUND: &str = "ground";

/// A plane under the site at the lowest level, a fifth wider than the model on every side.
fn ground(m: &Model, raw: &mut Vec<Raw>) {
    let (mut lo, mut hi) = ([f64::MAX; 2], [f64::MIN; 2]);
    for r in raw.iter() {
        for p in &r.pos {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    if lo[0] > hi[0] {
        return;
    }
    let pad = ((hi[0] - lo[0]).max(hi[1] - lo[1]) * 0.2).max(5.0);
    let z = m
        .level_list()
        .ok()
        .and_then(|l| l.iter().map(|x| x.2).min_by(f64::total_cmp))
        .unwrap_or(0.0)
        - 0.03;
    let (x0, y0, x1, y1) = (lo[0] - pad, lo[1] - pad, hi[0] + pad, hi[1] + pad);
    raw.push(Raw {
        table: GROUND.into(),
        id: 0,
        name: "Ground".into(),
        cat: "Ground".into(),
        level: None,
        rgba: [92, 104, 84, 255],
        tri_rgba: vec![],
        group: None,
        pos: vec![[x0, y0, z], [x1, y0, z], [x1, y1, z], [x0, y1, z]],
        tris: vec![[0, 1, 2], [0, 2, 3]],
    });
}

/// MEP runs: (domain, system kind) by id.
fn mep_runs(m: &Model) -> HashMap<i64, (String, Option<String>)> {
    let sql =
        "SELECT r.id, r.domain, s.kind FROM mep_run r LEFT JOIN mep_system s ON s.id=r.system";
    m.sql()
        .prepare(sql)
        .and_then(|mut st| {
            st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
                .collect()
        })
        .unwrap_or_default()
}

/// Systems the viewer shows and hides as a whole.
pub const GROUPS: [&str; 5] = ["HVAC", "Water", "Drainage", "Electrical", "Fire"];

/// Group and colour of an MEP system kind (`ddc_geom::mep::SystemKind::name`).
fn system_style(kind: Option<&str>) -> (Option<&'static str>, [u8; 4]) {
    let (g, c) = match kind.unwrap_or("") {
        "supply_air" => ("HVAC", [60, 130, 220]),
        "return_air" | "exhaust_air" => ("HVAC", [235, 140, 50]),
        "heating" => ("HVAC", [210, 70, 60]),
        "cold_water" | "hot_water" | "chilled_water" | "medical_gas" => ("Water", [60, 200, 220]),
        "sanitary" => ("Drainage", [135, 95, 55]),
        "power" => ("Electrical", [235, 205, 50]),
        "sprinkler" => ("Fire", [220, 50, 50]),
        _ => ("HVAC", [150, 160, 175]),
    };
    (Some(g), [c[0], c[1], c[2], 255])
}

/// System group of a library family by its name.
fn family_group(family: &str) -> Option<&'static str> {
    let f = family.to_ascii_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| f.contains(k));
    if has(&["air handling", "diffuser", "grille", "radiator", "fan coil"]) {
        Some("HVAC")
    } else if has(&["floor drain", "gully", "manhole"]) {
        Some("Drainage")
    } else if has(&["sprinkler", "extinguisher", "hydrant", "fire alarm"]) {
        Some("Fire")
    } else if has(&[
        "panel board",
        "switchboard",
        "socket",
        "luminaire",
        "light fitting",
    ]) {
        Some("Electrical")
    } else {
        None
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map_or(String::new(), |f| f.to_uppercase().chain(c).collect())
}

/// Colour of a component part's material: the project's or the library's, else one read from
/// the name (glass, planting, timber, paving, metal).
fn material_rgba(m: &Model, name: &str) -> Option<[u8; 4]> {
    if let Some(c) = m.material(name).ok().and_then(|x| x.spec.color) {
        return Some([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
    }
    let n = name.to_ascii_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| n.contains(k));
    let c = if has(&["glass", "mirror", "glaz"]) {
        [140, 185, 215]
    } else if has(&["foliage", "grass", "lawn", "leaf", "hedge", "moss"]) {
        [92, 140, 66]
    } else if has(&["bark", "trunk"]) {
        [105, 78, 55]
    } else if has(&["soil", "mulch"]) {
        [90, 68, 50]
    } else if has(&["oak", "walnut", "timber", "wood", "larch", "veneer"]) {
        [160, 118, 78]
    } else if has(&["paving", "asphalt", "tarmac", "gravel", "concrete", "stone"]) {
        [150, 150, 148]
    } else if has(&["steel", "alumin", "chrome", "metal"]) {
        [165, 170, 176]
    } else {
        return None;
    };
    Some([c[0], c[1], c[2], 255])
}

fn comp_rgba(family: &str) -> [u8; 4] {
    let f = family.to_ascii_lowercase();
    let has = |k: &[&str]| k.iter().any(|k| f.contains(k));
    let c = if has(&["window", "glaz"]) {
        [150, 190, 220]
    } else if has(&["door"]) {
        [176, 132, 92]
    } else if has(&[
        "lawn", "hedge", "planter", "tree", "shrub", "plant", "grass",
    ]) {
        [92, 140, 66]
    } else if has(&["paving", "road", "kerb", "curb", "path"]) {
        [150, 150, 148]
    } else {
        [196, 188, 172]
    };
    [c[0], c[1], c[2], 255]
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
        for (ti, t) in r.tris.iter().enumerate() {
            let rgba = r.tri_rgba.get(ti).copied().unwrap_or(r.rgba);
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
                s.col.push(rgba);
            }
        }
        let count = s.pos.len() as u32 - first;
        if count == 0 {
            continue;
        }
        // the ground plane is padded around the model; the fit is to the model
        for k in (0..3).filter(|_| r.table != GROUND) {
            s.lo[k] = s.lo[k].min(lo[k]);
            s.hi[k] = s.hi[k].max(hi[k]);
        }
        s.items.push(Item {
            table: r.table,
            id: r.id,
            name: r.name,
            cat: r.cat,
            level: r.level,
            group: r.group,
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
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
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
        self.items
            .iter()
            .position(|it| it.table == table && it.id == id)
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
        Value::Text(s) if s.chars().count() > 300 => {
            Some(format!("{}…", s.chars().take(300).collect::<String>()))
        }
        Value::Text(s) => Some(s.clone()),
        Value::Blob(b) => Some(format!("<{} bytes>", b.len())),
    }
}

/// Properties of an element: its stored row, then imported property sets.
pub fn properties(p: &Project, table: &str, id: i64) -> Vec<(String, String)> {
    let mut out = vec![];
    let db = p.model.sql();
    let known = [
        "wall",
        "slab",
        "roof",
        "col",
        "beam",
        "curtain_wall",
        "mass",
        "ceiling",
        "mep_run",
        "mesh",
        "stair",
        "railing",
        "comp",
        "room",
        "opening",
    ];
    if !known.contains(&table) {
        return out;
    }
    if let Ok(mut st) = db.prepare(&format!("SELECT * FROM {table} WHERE id=?1")) {
        let names: Vec<String> = st.column_names().map(|s| s.to_string()).collect();
        let row = st.query_row([id], |r| {
            (0..names.len())
                .map(|k| r.get::<_, Value>(k))
                .collect::<Result<Vec<_>, _>>()
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
            let t = if unit.is_empty() {
                t
            } else {
                format!("{t} {unit}")
            };
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
    let found =
        checks::run_all(&p.model, None, &CheckScope::All, limit).map_err(|e| e.to_string())?;
    Ok(found
        .into_iter()
        .map(|f| {
            let mut s = catalog()
                .get(f.msg_key)
                .cloned()
                .unwrap_or_else(|| f.msg_key.to_string());
            // some texts number their arguments ({0}, {1}) where the rule names them
            for (i, (k, v)) in f.args.iter().enumerate() {
                s = s
                    .replace(&format!("{{{k}}}"), v)
                    .replace(&format!("{{{i}}}"), v);
            }
            if let Some(ms) = f.measure {
                let unit = match ms.unit {
                    "m2" => "m²",
                    "m3" => "m³",
                    u => u,
                };
                s = format!(
                    "{s} (measured {:.3} {unit}, limit {:.3} {unit})",
                    ms.value, ms.limit
                );
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
