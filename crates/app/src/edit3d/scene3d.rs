//! Registry of 3D scene bodies: category to meshes plus pick targets. The scene base is `mep3d::scene`
//! (architecture with stable numbers, networks per system, components as instances); everything
//! else comes from [`SceneProvider`]s: roofs, ceilings, columns, beams, stairs,
//! railings, imported meshes and (for picking only) rooms. Other modules
//! add their categories via [`register`]; a provider with the same key replaces the built-in one.
//!
//! [`compose_with`] builds the scene of all levels from the model change journal: only providers
//! whose tables changed are rebuilt, and only the touched levels; other parts are the same
//! `Arc<SceneMesh>`, so GPU chunks and the scene index recognize them. Parts of the active level
//! get stable numbers `EXT_BASE + slot` (slot to target); other levels get `OTHER_LEVEL`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use ddc_model::Changes;
use ddc_render::glam::Vec3;
use ddc_render::scene::mesh_box;
use ddc_render::{Aabb, Scene, SceneMesh};

use super::math::{f3, rotate3, v3};
use super::pick::{bar, elev, ids, prism, push_mesh, shape_wh, Cat, Target};
use crate::editor::levels::OTHER_LEVEL;
use crate::editor::Editor;
pub use crate::scene_ids::{EXT_BASE, EXT_END};

/// Shader palette tags (`palette` in mesh.wgsl: 5-11 are special, `TAG_RGB | color` is a custom
/// color, others use `tag % 4`).
pub const TAG_ROOF: u32 = 1;
pub const TAG_CEILING: u32 = 2;
pub const TAG_MEMBER: u32 = 3;
pub const TAG_STAIR: u32 = 1;
/// A railing has its own color (steel), not the curtain-wall mullion tag (`TAG_MULLION`).
pub const TAG_RAILING: u32 = ddc_render::TAG_RGB | 0x6e_7278;
pub const TAG_MESH: u32 = 0;

/// Number of columns and beams per level up to which bodies are exact (otherwise bars from the type's section).
pub const EXACT_MEMBERS: usize = 400;

/// Element body: vertices in the scene frame (z from the active level elevation `ed.elevation`).
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub target: Target,
    pub tag: u32,
    pub pos: Vec<Vec3>,
    pub tris: Vec<[u32; 3]>,
}

/// A repeated body drawn as an instance: the shared mesh in its own frame (`key` names it,
/// equal keys are equal meshes) and the placement into the scene frame.
#[derive(Clone, Debug)]
pub struct InstPart {
    pub target: Target,
    pub tag: u32,
    pub key: u64,
    pub proto: Arc<SceneMesh>,
    pub m: ddc_render::glam::Mat4,
}

impl InstPart {
    /// The body as a plain part (scene frame vertices).
    pub fn plain(&self) -> Part {
        let mut p = Part::new(self.target, self.tag);
        p.pos = self.proto.positions.iter().map(|q| self.m.transform_point3(Vec3::from_array(*q))).collect();
        p.tris = self.proto.triangles.clone();
        p
    }
}

impl Part {
    pub fn new(target: Target, tag: u32) -> Part {
        Part {
            target,
            tag,
            pos: vec![],
            tris: vec![],
        }
    }

    /// Add a model mesh (absolute elevations).
    pub fn push(&mut self, m: &ddc_geom::mesh::Mesh, z0: f64) {
        push_mesh(&mut self.pos, &mut self.tris, m, z0);
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }
}

/// Provider of a category's bodies.
pub trait SceneProvider: Send + Sync {
    /// Unique key (`"column"`, `"footing"`, ...): the same key replaces the built-in provider.
    fn key(&self) -> &'static str;
    /// Bodies of `level`'s elements in the scene frame (z from `ed.elevation`).
    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part>;
    /// What the scene draws: plain parts and repeated bodies as instances (the same bodies as
    /// [`Self::parts`], which picking and exports read).
    fn parts_inst(&self, ed: &Editor, level: i64) -> (Vec<Part>, Vec<InstPart>) {
        (self.parts(ed, level), vec![])
    }
    /// For picking only (not drawn): rooms, volumes.
    fn pick_only(&self) -> bool {
        false
    }
    /// Pick rank: 0 is elements, 1 is volumes behind elements.
    fn rank(&self) -> u8 {
        0
    }
    /// Model tables the bodies depend on; empty means rebuild after any edit.
    fn tables(&self) -> &'static [&'static str] {
        &[]
    }
    /// Main table (row `id` equals target `id`, has a `level` column): editing only its
    /// rows rebuilds just their levels.
    fn main_table(&self) -> Option<&'static str> {
        None
    }
    /// Levels with changed bodies after an edit of `tables()` (`None` means by `main_table`):
    /// the provider compares its own fingerprints; an empty list means no rebuild is needed.
    fn dirty_levels(&self, _ed: &Editor) -> Option<Vec<i64>> {
        None
    }
}

struct Builtin {
    key: &'static str,
    f: fn(&Editor, i64) -> Vec<Part>,
    pick_only: bool,
    tables: &'static [&'static str],
    main: Option<&'static str>,
}

impl SceneProvider for Builtin {
    fn key(&self) -> &'static str {
        self.key
    }
    fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
        (self.f)(ed, level)
    }
    fn pick_only(&self) -> bool {
        self.pick_only
    }
    fn rank(&self) -> u8 {
        u8::from(self.pick_only)
    }
    fn tables(&self) -> &'static [&'static str] {
        self.tables
    }
    fn main_table(&self) -> Option<&'static str> {
        self.main
    }
}

const fn b(
    key: &'static str,
    f: fn(&Editor, i64) -> Vec<Part>,
    tables: &'static [&'static str],
) -> Builtin {
    Builtin {
        key,
        f,
        pick_only: false,
        tables,
        main: Some(tables[0]),
    }
}

/// Networks and components are drawn by `mep3d::scene` (one build, instances), so they are absent here.
static BUILTIN: [Builtin; 14] = [
    b(
        "roof",
        roofs,
        &[
            "roof",
            "roof_vertex",
            "slab_type",
            "slab_type_layer",
            "shaft",
            "shaft_cut",
            "shaft_vertex",
            // copings and downpipes follow the parapet walls and the ground level
            "wall",
            "wall_type_layer",
            "level",
        ],
    ),
    b(
        "ceiling",
        ceilings,
        &["ceiling", "ceiling_vertex", "slab_type", "slab_type_layer"],
    ),
    b("column", columns, &["col", "member_type"]),
    b("beam", beams, &["beam", "member_type", "col"]),
    b("stair", stairs, &["stair"]),
    b("railing", railings, &["railing", "railing_vertex", "stair"]),
    b("mesh", imported, &["mesh"]),
    b(
        "shaft",
        super::zone_shaft3d::shafts,
        &["shaft", "shaft_vertex", "level"],
    ),
    // a zone row has no level: any change of its tables rebuilds the markers
    Builtin {
        key: "zone",
        f: super::zone_shaft3d::zones,
        pick_only: false,
        tables: &["zone", "zone_room", "room", "level"],
        main: None,
    },
    // datums belong to no level: the provider draws them on the active level only
    Builtin {
        key: "grid",
        f: crate::elem::datum::grids3d,
        pick_only: false,
        tables: &["grid"],
        main: None,
    },
    Builtin {
        key: "ref_plane",
        f: crate::elem::datum::ref_planes3d,
        pick_only: false,
        tables: &["ref_plane"],
        main: None,
    },
    b("room_sep", super::room3d::seps, &["room_sep", "level"]),
    // the pad follows walls and room points: any change rebuilds it
    Builtin {
        key: "room_pad",
        f: super::room3d::pads,
        pick_only: false,
        tables: &[],
        main: None,
    },
    Builtin {
        key: "room",
        f: rooms,
        pick_only: true,
        tables: &[],
        main: None,
    },
];

static EXTRA: RwLock<Vec<Arc<dyn SceneProvider>>> = RwLock::new(Vec::new());

/// Register a provider; registering the same key again replaces the previous one.
pub fn register(p: Arc<dyn SceneProvider>) {
    if let Ok(mut v) = EXTRA.write() {
        v.retain(|q| q.key() != p.key());
        v.push(p);
    }
}

/// A provider with this key is connected.
pub fn provided(key: &str) -> bool {
    EXTRA.read().is_ok_and(|v| v.iter().any(|p| p.key() == key))
}

/// Iterate providers: registered ones, then the built-in ones that were not replaced.
pub fn each(mut f: impl FnMut(&dyn SceneProvider)) {
    let extra: Vec<Arc<dyn SceneProvider>> = EXTRA.read().map(|v| v.clone()).unwrap_or_default();
    for p in &extra {
        f(p.as_ref());
    }
    for p in BUILTIN.iter() {
        if !extra.iter().any(|q| q.key() == p.key) {
            f(p as &dyn SceneProvider);
        }
    }
}

/// All bodies of a level (drawn and/or pick-only).
pub fn parts(ed: &Editor, level: i64, drawn: bool, pick_only: bool) -> Vec<(Part, u8)> {
    let mut out = vec![];
    each(|p| {
        if (p.pick_only() && pick_only) || (!p.pick_only() && drawn) {
            let r = p.rank();
            out.extend(
                p.parts(ed, level)
                    .into_iter()
                    .filter(|q| !q.is_empty())
                    .map(|q| (q, r)),
            );
        }
    });
    out
}

/// Part builds kept between frames (lives in `Edit3d`).
#[derive(Default)]
pub struct Cache {
    /// (active level, its elevation): a different one means rebuild everything (numbers and z are baked into the meshes).
    key: Option<(i64, u64)>,
    head: Option<u64>,
    /// (provider, level) to part meshes and their bounds.
    parts: HashMap<(&'static str, i64), Vec<(Arc<SceneMesh>, Aabb, Target)>>,
    /// (provider, level) to instanced bodies and their element numbers.
    insts: HashMap<(&'static str, i64), Vec<(InstPart, u32)>>,
    /// (provider, target id) to the level where the part was built (a deleted row cannot tell its level).
    at: HashMap<(&'static str, i64), i64>,
    slot: HashMap<Target, u32>,
    targets: Vec<Target>,
    /// Rebuilt in the last build (provider, level).
    pub rebuilt: Vec<(&'static str, i64)>,
    /// Instanced mesh cache (`mesh_inst`) and the slot-map reset counter.
    pub inst: super::mesh_inst::Feed,
    slot_epoch: u64,
}

enum Dirty {
    None,
    All,
    Levels(HashSet<i64>),
}

fn dirty(p: &dyn SceneProvider, ch: &Changes, c: &Cache, ed: &Editor) -> Dirty {
    if ch.is_empty() {
        return Dirty::None;
    }
    let tables = p.tables();
    if tables.is_empty() {
        return Dirty::All;
    }
    if !tables.iter().any(|t| ch.touched(t)) {
        return Dirty::None;
    }
    if !ch.all {
        if let Some(ls) = p.dirty_levels(ed) {
            return Dirty::Levels(ls.into_iter().collect());
        }
    }
    let Some(main) = p.main_table() else {
        return Dirty::All;
    };
    if ch.whole(main) || tables.iter().any(|t| *t != main && ch.touched(t)) {
        return Dirty::All;
    }
    let k = p.key();
    let mut out = HashSet::new();
    for id in ch.ids(main) {
        if let Some(l) = c.at.get(&(k, id)) {
            out.insert(*l);
        }
        let now: Option<Option<i64>> = ed
            .model
            .sql()
            .query_row(
                &format!("SELECT level FROM {main} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )
            .ok();
        match now {
            Some(Some(l)) => {
                out.insert(l);
            }
            Some(None) => {
                out.insert(ed.level);
            }
            None => {}
        }
    }
    Dirty::Levels(out)
}

fn slot(c: &mut Cache, t: Target) -> u32 {
    if let Some(&s) = c.slot.get(&t) {
        return EXT_BASE + s;
    }
    let s = c.targets.len() as u32;
    c.targets.push(t);
    c.slot.insert(t, s);
    EXT_BASE + s
}

/// Scene of all levels without a cache (tests, one-off snapshots).
pub fn compose(ed: &mut Editor) -> (Scene, Vec<Target>) {
    // a one-off snapshot without instance feeding: the provider draws the meshes, the live feed is unaffected
    let live = std::mem::take(&mut ed.big.instanced_meshes);
    let out = compose_with(ed, &mut Cache::default());
    ed.big.instanced_meshes = live;
    out
}

/// Scene of all levels plus the active level's part targets (element `EXT_BASE + k` maps to `targets[k]`).
pub fn compose_with(ed: &mut Editor, c: &mut Cache) -> (Scene, Vec<Target>) {
    // roofs, columns, beams, ceilings and meshes are drawn by the providers below, not by the editor scene;
    // solids too (repeated ones as instances), so their provider is always connected here
    if !provided("solid") {
        crate::solid_edit3d::install();
    }
    ed.scene_providers = true;
    let span = ddc_model::perf::span("compose.base");
    let mut base = crate::mep3d::scene(ed);
    drop(span);
    ed.scene_providers = false;
    let ch = match c.head {
        Some(h) => ed.model.changes_since(h),
        None => Changes {
            all: true,
            ..Default::default()
        },
    };
    if c.inst.sync(ed, &ch) {
        c.parts.retain(|k, _| k.0 != "mesh");
        c.at.retain(|k, _| k.0 != "mesh");
    }
    let ed: &Editor = ed;
    let key = (ed.level, ed.elevation.to_bits());
    if c.key != Some(key) || c.targets.len() > (crate::scene_ids::SPAN / 2) as usize {
        c.slot.clear();
        c.slot_epoch += 1;
        c.targets.clear();
        c.parts.clear();
        c.insts.clear();
        c.at.clear();
    }
    if ch.all || ch.touched("level") {
        c.parts.clear();
        c.insts.clear();
        c.at.clear();
    }
    let cur = ed.level;
    let mut levels: Vec<i64> = if ed.scene_one_level {
        vec![]
    } else {
        ed.levels().into_iter().map(|l| l.0).collect()
    };
    if !levels.contains(&cur) {
        levels.push(cur);
    }
    c.parts.retain(|k, _| levels.contains(&k.1));
    c.insts.retain(|k, _| levels.contains(&k.1));
    c.rebuilt.clear();
    let mut order: Vec<&'static str> = vec![];
    each(|p| {
        if p.pick_only() {
            return;
        }
        let k = p.key();
        order.push(k);
        let _ps = ddc_model::perf::span(k);
        let d = dirty(p, &ch, c, ed);
        for &lv in &levels {
            let stale = match &d {
                Dirty::None => false,
                Dirty::All => true,
                Dirty::Levels(s) => s.contains(&lv),
            };
            if !stale && c.parts.contains_key(&(k, lv)) {
                continue;
            }
            c.at.retain(|(pk, _), l| !(*pk == k && *l == lv));
            let mut v = vec![];
            let (plain, inst) = p.parts_inst(ed, lv);
            let mut iv = Vec::with_capacity(inst.len());
            for q in inst {
                let element = if lv == cur { slot(c, q.target) } else { OTHER_LEVEL };
                c.at.insert((k, q.target.id), lv);
                iv.push((q, element));
            }
            c.insts.insert((k, lv), iv);
            for p in plain.into_iter().filter(|q| !q.is_empty()) {
                let target = p.target;
                let element = if lv == cur {
                    slot(c, target)
                } else {
                    OTHER_LEVEL
                };
                c.at.insert((k, target.id), lv);
                let m = SceneMesh {
                    element,
                    tag: p.tag,
                    positions: p.pos.iter().map(|v| v.to_array()).collect(),
                    triangles: p.tris,
                };
                let b = mesh_box(&m);
                v.push((Arc::new(m), b, target));
            }
            c.parts.insert((k, lv), v);
            c.rebuilt.push((k, lv));
        }
    });
    c.parts.retain(|k, _| order.contains(&k.0));
    c.insts.retain(|k, _| order.contains(&k.0));
    let n = base.meshes.len() + c.parts.values().map(Vec::len).sum::<usize>();
    let (mut meshes, mut boxes) = (Vec::with_capacity(n), Vec::with_capacity(n));
    meshes.extend(base.meshes.iter().cloned());
    boxes.extend_from_slice(base.boxes());
    // the own level is filtered by `filter` and `paint_scene`; a foreign one has no element number, so here
    let gfx = crate::view_gfx_ui::layer(ed, ddc_model::ViewKey::ThreeD);
    let texl = crate::mat_tex::layers(ed);
    for k in &order {
        for lv in &levels {
            for (m, b, t) in c.parts.get(&(*k, *lv)).into_iter().flatten() {
                let tag = if *lv == cur {
                    Some(m.tag)
                } else {
                    gfx.part_tag(*t, m.tag)
                };
                let Some(tag) = tag else { continue };
                // imported meshes take the material of the element or of its category
                let table = t
                    .grip_kind()
                    .and_then(crate::view_gfx_ui::kind_table)
                    .or_else(|| (t.cat == Cat::Mesh).then_some("mesh"))
                    .filter(|_| !texl.is_empty());
                if let Some(parts) = table.and_then(|tb| texl.split(tb, t.id, m, tag)) {
                    for p in parts {
                        boxes.push(mesh_box(&p));
                        meshes.push(Arc::new(p));
                    }
                    continue;
                }
                let tag = match table {
                    Some(table) => texl.elem(table, t.id, tag),
                    None => tag,
                };
                if tag == m.tag {
                    meshes.push(m.clone());
                } else {
                    let mut own = (**m).clone();
                    own.tag = tag;
                    meshes.push(Arc::new(own));
                }
                boxes.push(*b);
            }
        }
    }
    // instanced bodies: same tag rules as the parts; a textured body is drawn plain
    let mut groups: std::collections::BTreeMap<(&'static str, u64, u32), (Arc<SceneMesh>, Vec<ddc_render::Inst>)> =
        Default::default();
    for k in &order {
        for lv in &levels {
            for (q, element) in c.insts.get(&(*k, *lv)).into_iter().flatten() {
                let t = q.target;
                let tag = if *lv == cur { Some(q.tag) } else { gfx.part_tag(t, q.tag) };
                let Some(tag) = tag else { continue };
                let table = t
                    .grip_kind()
                    .and_then(crate::view_gfx_ui::kind_table)
                    .or_else(|| (t.cat == Cat::Mesh).then_some("mesh"))
                    .filter(|_| !texl.is_empty());
                let tag = match table {
                    Some(tb) => {
                        let p = q.plain();
                        let m = SceneMesh {
                            element: *element,
                            tag: q.tag,
                            positions: p.pos.iter().map(|v| v.to_array()).collect(),
                            triangles: p.tris,
                        };
                        if let Some(parts) = texl.split(tb, t.id, &m, tag) {
                            for p in parts {
                                boxes.push(mesh_box(&p));
                                meshes.push(Arc::new(p));
                            }
                            continue;
                        }
                        texl.elem(tb, t.id, tag)
                    }
                    None => tag,
                };
                let g = groups.entry((*k, q.key, tag)).or_insert_with(|| {
                    let proto = if q.proto.tag == tag {
                        q.proto.clone()
                    } else {
                        Arc::new(SceneMesh { tag, ..(*q.proto).clone() })
                    };
                    (proto, vec![])
                });
                g.1.push(ddc_render::Inst { m: q.m, element: *element });
            }
        }
    }
    let mut inst = std::mem::take(&mut base.instanced);
    for ((k, key, tag), (proto, v)) in groups {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(k, key, tag, "scene3d.inst"), &mut h);
        inst.push(ddc_render::Instanced::new(std::hash::Hasher::finish(&h), proto, v));
    }
    let map = super::mesh_inst::MapKey {
        active: cur,
        z0: ed.elevation,
        levels: levels.clone(),
        epoch: c.slot_epoch,
    };
    let mut feed = std::mem::take(&mut c.inst);
    inst.extend(feed.groups(map, &mut |id| slot(c, Target::new(Cat::Mesh, id))));
    c.inst = feed;
    c.head = Some(ed.model.change_head());
    c.key = Some(key);
    (
        Scene::from_parts(meshes, boxes).with_instanced(inst),
        c.targets.clone(),
    )
}

/// Target of a scene element from provider parts: Some(None) means a number from their range with no target.
pub fn target_of(element: u32, targets: &[Target]) -> Option<Option<Target>> {
    (EXT_BASE..=EXT_END)
        .contains(&element)
        .then(|| targets.get((element - EXT_BASE) as usize).copied())
}

/// Category of a scene part (key `e3d.cat.*`), for the view's category filters.
pub fn category_of(element: u32, targets: &[Target]) -> Option<&'static str> {
    target_of(element, targets).flatten().map(|t| t.cat.key())
}

/// Hide/isolate with provider parts: a column, roof, ... is handled by its own target.
pub fn filter(vis: &crate::isolate::Vis, ed: &Editor, sc: Scene, targets: &[Target]) -> Scene {
    vis.filter_scene_with(ed, sc, |e| {
        let t = target_of(e, targets).flatten()?;
        let k = t.grip_kind()?;
        Some(vis.part_visible((k, t.id)))
    })
}

// ---- built-in providers ----

fn one(cat: Cat, tag: u32, id: i64, ms: &[ddc_geom::mesh::Mesh], z0: f64) -> Part {
    let mut p = Part::new(Target::new(cat, id), tag);
    for m in ms {
        p.push(m, z0);
    }
    p
}

/// Roof faces take the colour of their deck finish (top layer of the type) when it is one.
fn roof_tag(m: &ddc_model::Model, id: i64) -> u32 {
    let sql = "SELECT l.material FROM roof r JOIN slab_type_layer l ON l.stype=r.stype \
               WHERE r.id=?1 ORDER BY l.pos LIMIT 1";
    m.sql()
        .query_row(sql, [id], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|s| crate::editor::opening_fill::finish_rgb(&s))
        .map_or(TAG_ROOF, |c| ddc_render::tag_rgb(c, false))
}

fn roofs(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    let mut out: Vec<Part> = ids(m, "SELECT id FROM roof WHERE level=?1", lv)
        .into_iter()
        .filter_map(|id| {
            m.roof_mesh(id)
                .ok()
                .map(|ms| one(Cat::Roof, roof_tag(m, id), id, &[ms], ed.elevation))
        })
        .collect();
    out.extend(super::detail3d::roof_edges(ed, lv));
    out
}

fn stairs(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    ids(m, "SELECT id FROM stair WHERE level=?1", lv)
        .into_iter()
        .filter_map(|id| {
            let mut p = m
                .stair_meshes(id)
                .ok()
                .map(|ms| one(Cat::Stair, TAG_STAIR, id, &ms, ed.elevation))?;
            super::detail3d::stair_nosings(ed, id, &mut p);
            Some(p)
        })
        .collect()
}

fn railings(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    ids(m, "SELECT id FROM railing WHERE level=?1", lv)
        .into_iter()
        .filter_map(|id| {
            super::detail3d::railing(ed, id).or_else(|| {
                m.railing_meshes(id)
                    .ok()
                    .map(|ms| one(Cat::Railing, TAG_RAILING, id, &ms, ed.elevation))
            })
        })
        .collect()
}

fn ceilings(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    let mut out = vec![];
    for id in ids(m, "SELECT id FROM ceiling WHERE level=?1", lv) {
        let row: Option<(f64, Option<f64>)> = m
            .sql()
            .query_row(
                "SELECT c.height, (SELECT SUM(thickness) FROM slab_type_layer WHERE stype=c.stype) \
                 FROM ceiling c WHERE c.id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        let Some((h, t)) = row else { continue };
        let mesh = m
            .ceiling_rings(id)
            .ok()
            .and_then(|rings| prism(&rings, elev(m, lv) + h, t.unwrap_or(0.02)));
        if let Some(mesh) = mesh {
            out.push(one(Cat::Ceiling, TAG_CEILING, id, &[mesh], ed.elevation));
        }
    }
    out
}

fn member_sizes(m: &ddc_model::Model) -> HashMap<i64, (f64, f64)> {
    m.member_types()
        .unwrap_or_default()
        .into_iter()
        .map(|t| (t.0, shape_wh(&t.2)))
        .collect()
}

fn exact_members(m: &ddc_model::Model, lv: i64) -> bool {
    let n: i64 = m
        .sql()
        .query_row(
            "SELECT (SELECT COUNT(*) FROM col WHERE level=?1) + (SELECT COUNT(*) FROM beam WHERE level=?1)",
            [lv],
            |r| r.get(0),
        )
        .unwrap_or(0);
    n as usize <= EXACT_MEMBERS
}

fn columns(ed: &Editor, lv: i64) -> Vec<Part> {
    let (m, z0) = (&ed.model, ed.elevation);
    let exact = exact_members(m, lv);
    let sizes = member_sizes(m);
    let (shapes, lod) = (super::detail3d::member_shapes(m), super::detail3d::lod(ed));
    let mut out = vec![];
    for id in ids(m, "SELECT id FROM col WHERE level=?1", lv) {
        if exact {
            if let Ok(mesh) = m.member_mesh("column", id) {
                out.push(one(Cat::Column, TAG_MEMBER, id, &[mesh], z0));
                continue;
            }
        }
        let Ok(c) = m.column(id) else { continue };
        let zb = elev(m, c.level) + c.base_offset - z0;
        let zt = elev(m, c.top_level.unwrap_or(c.level)) + c.top_offset - z0;
        let top = c.top_at.unwrap_or(c.at);
        let (w, h) = sizes.get(&c.member_type).copied().unwrap_or((0.3, 0.3));
        let up = Vec3::new(c.rotation.cos() as f32, c.rotation.sin() as f32, 0.0);
        let a = Vec3::new(c.at.0 as f32, c.at.1 as f32, zb as f32);
        let b = Vec3::new(top.0 as f32, top.1 as f32, zt as f32);
        // the section's depth runs across the rotation direction, as the block's did
        let (pos, tris) = match shapes.get(&c.member_type) {
            Some(s) => super::detail3d::section_bar(s, a, b, (b - a).normalize_or(Vec3::Z).cross(up), lod),
            None => bar(a, b, h as f32, w as f32, up),
        };
        out.push(Part {
            target: Target::new(Cat::Column, id),
            tag: TAG_MEMBER,
            pos,
            tris,
        });
    }
    out
}

/// Beam bar from its section: `rotation` about the axis and the `yjust`/`zjust` justifications.
pub fn beam_bar(bm: &ddc_model::BeamSpec, e: f64, w: f64, h: f64) -> (Vec<Vec3>, Vec<[u32; 3]>) {
    let (a, b, up) = beam_frame(bm, e, w, h);
    bar(a, b, w as f32, h as f32, up)
}

/// Ends of a beam's body (the axis shifted by its justification) and the section's up.
pub fn beam_frame(bm: &ddc_model::BeamSpec, e: f64, w: f64, h: f64) -> (Vec3, Vec3, Vec3) {
    let p = |q: (f64, f64, f64)| Vec3::new(q.0 as f32, q.1 as f32, (q.2 + e) as f32);
    let (a, b) = (p(bm.a), p(bm.b));
    let (axis, zero) = super::grips::roll_frame(a, b);
    let up = v3(rotate3(f3(zero), [0.0; 3], f3(axis), bm.rotation)).normalize_or(Vec3::Z);
    let y = up.cross(axis).normalize_or(Vec3::Y);
    // the beam line is at a face: the body is offset from it by half the section
    let shift = |j: ddc_model::Just, s: f64| match j {
        ddc_model::Just::Pos => -s * 0.5,
        ddc_model::Just::Neg => s * 0.5,
        _ => 0.0,
    };
    let off = y * shift(bm.yjust, w) as f32 + up * shift(bm.zjust, h) as f32;
    (a + off, b + off, up)
}

fn beams(ed: &Editor, lv: i64) -> Vec<Part> {
    let (m, z0) = (&ed.model, ed.elevation);
    let exact = exact_members(m, lv);
    let sizes = member_sizes(m);
    let (shapes, lod) = (super::detail3d::member_shapes(m), super::detail3d::lod(ed));
    let mut out = vec![];
    for id in ids(m, "SELECT id FROM beam WHERE level=?1", lv) {
        if exact {
            if let Ok(mesh) = m.member_mesh("beam", id) {
                out.push(one(Cat::Beam, TAG_MEMBER, id, &[mesh], z0));
                continue;
            }
        }
        let Ok(bm) = m.beam(id) else { continue };
        let e = elev(m, bm.level) - z0;
        let (w, h) = sizes.get(&bm.member_type).copied().unwrap_or((0.2, 0.4));
        let (pos, tris) = match shapes.get(&bm.member_type) {
            Some(s) => {
                let (a, b, up) = beam_frame(&bm, e, w, h);
                super::detail3d::section_bar(s, a, b, up, lod)
            }
            None => beam_bar(&bm, e, w, h),
        };
        out.push(Part {
            target: Target::new(Cat::Beam, id),
            tag: TAG_MEMBER,
            pos,
            tris,
        });
    }
    out
}

/// Imported meshes of a level (`ddc_model::imported`); meshes with no level go with the active one.
fn imported(ed: &Editor, lv: i64) -> Vec<Part> {
    let m = &ed.model;
    let skip = &ed.big.instanced_meshes;
    let mut ms = if skip.is_empty() {
        m.imported_meshes(Some(lv)).unwrap_or_default()
    } else {
        // bodies are not unpacked from the cache: opening a large model depends on that
        ids(m, "SELECT id FROM mesh WHERE level=?1 ORDER BY id", lv)
            .into_iter()
            .filter(|id| !skip.contains(id))
            .filter_map(|id| m.imported_mesh(id).ok())
            .collect()
    };
    if lv == ed.level {
        for id in ids(
            m,
            "SELECT id FROM mesh WHERE level IS NULL AND ?1 IS NOT NULL",
            lv,
        ) {
            if skip.contains(&id) {
                continue;
            }
            if let Ok(x) = m.imported_mesh(id) {
                ms.push(x);
            }
        }
    }
    ms.into_iter()
        .map(|x| {
            one(
                Cat::Mesh,
                TAG_MESH,
                x.id,
                std::slice::from_ref(&x.mesh),
                ed.elevation,
            )
        })
        .collect()
}

/// Rooms of the active level: a volume from outline and height (picking only).
fn rooms(ed: &Editor, lv: i64) -> Vec<Part> {
    if lv != ed.level {
        return vec![];
    }
    let m = &ed.model;
    let mut out = vec![];
    for r in &ed.rooms {
        let h: f64 = m
            .sql()
            .query_row("SELECT height FROM room WHERE id=?1", [r.id], |x| x.get(0))
            .unwrap_or(2.7);
        let rings: Vec<Vec<(f64, f64)>> = r
            .rings
            .iter()
            .map(|ring| ring.iter().map(|p| (p.x, p.y)).collect())
            .collect();
        if let Some(mesh) = prism(&rings, 0.0, h) {
            out.push(one(Cat::Room, 0, r.id, &[mesh], 0.0));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        // other tests (own threads) do not see the provider
        static PROBE_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    struct Probe;
    impl SceneProvider for Probe {
        fn key(&self) -> &'static str {
            "test_probe"
        }
        fn parts(&self, ed: &Editor, level: i64) -> Vec<Part> {
            if level != ed.level || !PROBE_ON.with(|c| c.get()) {
                return vec![];
            }
            let (pos, tris) = bar(
                Vec3::new(50.0, 50.0, 0.0),
                Vec3::new(50.0, 50.0, 1.0),
                0.2,
                0.2,
                Vec3::X,
            );
            vec![Part {
                target: Target::new(Cat::Ext(7), 42),
                tag: 0,
                pos,
                tris,
            }]
        }
    }

    fn column(ed: &mut Editor, level: i64, at: (f64, f64)) -> i64 {
        let mt = match ed.model.member_types().unwrap().first() {
            Some(t) => t.0,
            None => ed.model.add_catalog_member_type("R 400x400").unwrap(),
        };
        let id = ed
            .model
            .add_column(&ddc_model::ColumnSpec {
                level,
                top_level: None,
                member_type: mt,
                structural: true,
                at,
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        ed.reload().unwrap();
        id
    }

    #[test]
    fn target_range() {
        let ts = [Target::new(Cat::Roof, 3)];
        assert_eq!(target_of(EXT_BASE, &ts), Some(Some(ts[0])));
        assert_eq!(target_of(EXT_BASE + 1, &ts), Some(None));
        assert_eq!(target_of(5, &ts), None);
        assert_eq!(target_of(OTHER_LEVEL, &ts), None);
        // u6 numbers (architecture, networks, components) are not provider parts
        for e in [
            crate::mep3d::ARCH_BASE,
            crate::mep3d::MEP_BASE,
            crate::mep3d::COMP_BASE,
        ] {
            assert_eq!(target_of(e, &ts), None);
        }
        assert_eq!(category_of(EXT_BASE, &ts), Some("roof"));
    }

    #[test]
    fn railing_tag_is_its_own() {
        assert_ne!(TAG_RAILING, ddc_render::TAG_MULLION);
        for t in [TAG_ROOF, TAG_CEILING, TAG_MEMBER, TAG_STAIR, TAG_MESH] {
            assert_ne!(TAG_RAILING, t);
        }
        assert!(!ddc_render::is_translucent(TAG_RAILING));
    }

    #[test]
    fn columns_beams_roofs_and_providers_are_in_the_scene_and_pickable() {
        PROBE_ON.with(|c| c.set(true));
        register(Arc::new(Probe));
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let col = column(&mut ed, lv, (2.0, 2.0));
        let (sc, ts) = compose(&mut ed);
        let k = ts
            .iter()
            .position(|t| *t == Target::new(Cat::Column, col))
            .expect("column in the scene");
        assert!(sc
            .meshes
            .iter()
            .any(|m| m.element == EXT_BASE + k as u32 && !m.triangles.is_empty()));
        assert!(
            ts.contains(&Target::new(Cat::Ext(7), 42)),
            "registered provider rendered"
        );
        // picking by scene body, without duplicates from providers
        let bodies = super::super::pick::Bodies::build_with(&ed, &sc, &ts);
        let n = bodies
            .list
            .iter()
            .filter(|b| b.target == Some(Target::new(Cat::Column, col)))
            .count();
        assert_eq!(n, 1);
        assert!(bodies.of(Target::new(Cat::Ext(7), 42)).is_some());
        PROBE_ON.with(|c| c.set(false));
    }

    #[test]
    fn compose_is_incremental_and_keeps_arcs() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let up = ed.model.add_level("L2", 3.0).unwrap();
        ed.reload().unwrap();
        let a = column(&mut ed, lv, (0.0, 0.0));
        let _b = column(&mut ed, up, (5.0, 0.0));
        let mut c = Cache::default();
        let (s1, t1) = compose_with(&mut ed, &mut c);
        assert!(c.rebuilt.contains(&("column", lv)) && c.rebuilt.contains(&("column", up)));
        let ka = t1
            .iter()
            .position(|t| *t == Target::new(Cat::Column, a))
            .unwrap() as u32;
        let ptr = |s: &Scene, el: u32| {
            s.meshes
                .iter()
                .find(|m| m.element == el)
                .map(|m| Arc::as_ptr(m) as usize)
        };
        let other = |s: &Scene| {
            s.meshes
                .iter()
                .filter(|m| m.element == OTHER_LEVEL && m.tag == TAG_MEMBER)
                .map(|m| Arc::as_ptr(m) as usize)
                .collect::<Vec<_>>()
        };
        // without edits nothing is rebuilt: the same meshes
        let (s2, _) = compose_with(&mut ed, &mut c);
        assert!(c.rebuilt.is_empty(), "{:?}", c.rebuilt);
        assert_eq!(ptr(&s1, EXT_BASE + ka), ptr(&s2, EXT_BASE + ka));
        // editing a column on the active level rebuilds only that level's columns
        let mut cs = ed.model.column(a).unwrap();
        cs.top_offset = 2.5;
        ed.model.update_column(a, &cs).unwrap();
        ed.reload().unwrap();
        let (s3, t3) = compose_with(&mut ed, &mut c);
        // beams depend on columns (`tables`); the test probe is always rebuilt
        let cols: Vec<_> = c
            .rebuilt
            .iter()
            .filter(|r| r.0 == "column")
            .copied()
            .collect();
        assert_eq!(cols, vec![("column", lv)]);
        assert_eq!(t3[ka as usize], Target::new(Cat::Column, a), "slot stays");
        assert_ne!(ptr(&s2, EXT_BASE + ka), ptr(&s3, EXT_BASE + ka));
        assert_eq!(other(&s2), other(&s3), "the other level keeps its meshes");
        assert!(!other(&s3).is_empty());
    }

    #[test]
    fn mep_and_components_are_not_doubled() {
        let mut ed = Editor::new_in_memory().unwrap();
        crate::mep3d::demo(&mut ed).unwrap();
        let (sc, ts) = compose(&mut ed);
        assert!(
            ts.iter().all(|t| !matches!(t.cat, Cat::Mep | Cat::Comp)),
            "{ts:?}"
        );
        assert!(sc
            .meshes
            .iter()
            .any(|m| (crate::mep3d::MEP_BASE..crate::mep3d::COMP_BASE).contains(&m.element)));
        let bodies = super::super::pick::Bodies::build_with(&ed, &sc, &ts);
        let run = *ed
            .model
            .mep_run_info()
            .unwrap()
            .keys()
            .min()
            .expect("demo has runs");
        let n = bodies
            .list
            .iter()
            .filter(|b| b.target == Some(Target::new(Cat::Mep, run)))
            .count();
        assert_eq!(n, 1, "one body per run, from the u6 scene");
    }

    #[test]
    fn hidden_column_part_leaves_the_scene() {
        let mut ed = Editor::new_in_memory().unwrap();
        let lv = ed.level;
        let a = column(&mut ed, lv, (0.0, 0.0));
        let b = column(&mut ed, lv, (3.0, 0.0));
        let (sc, ts) = compose(&mut ed);
        let el = |id: i64| {
            EXT_BASE
                + ts.iter()
                    .position(|t| *t == Target::new(Cat::Column, id))
                    .unwrap() as u32
        };
        let mut vis = crate::isolate::Vis::default();
        vis.hide_parts(&[], &[(crate::grips::Kind::Column, a)]);
        let f = filter(&vis, &ed, sc, &ts);
        assert!(!f.meshes.iter().any(|m| m.element == el(a)));
        assert!(f.meshes.iter().any(|m| m.element == el(b)));
        let (sc, ts) = compose(&mut ed);
        let mut vis = crate::isolate::Vis::default();
        vis.isolate_parts(&ed, &[], &[(crate::grips::Kind::Column, b)]);
        let f = filter(&vis, &ed, sc, &ts);
        assert!(
            f.meshes.iter().all(|m| m.element == el(b)),
            "isolation keeps only the column"
        );
    }

    #[test]
    fn beam_fallback_body_follows_roll() {
        let spec = ddc_model::BeamSpec {
            level: 1,
            member_type: 0,
            a: (0.0, 0.0, 3.0),
            b: (4.0, 0.0, 3.0),
            bulge: 0.0,
            yjust: ddc_model::Just::Center,
            zjust: ddc_model::Just::Center,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: ddc_model::BeamJoin::default(),
            join_end: ddc_model::BeamJoin::default(),
        };
        let zspan = |p: &[Vec3]| {
            let (lo, hi) = p
                .iter()
                .fold((f32::MAX, f32::MIN), |a, q| (a.0.min(q.z), a.1.max(q.z)));
            hi - lo
        };
        let (p0, _) = beam_bar(&spec, 0.0, 0.2, 0.4);
        assert!((zspan(&p0) - 0.4).abs() < 1e-4);
        let rolled = ddc_model::BeamSpec {
            rotation: std::f64::consts::FRAC_PI_2,
            ..spec.clone()
        };
        let (p1, _) = beam_bar(&rolled, 0.0, 0.2, 0.4);
        assert!(
            (zspan(&p1) - 0.2).abs() < 1e-4,
            "rolled 90°: width is vertical"
        );
        let low = ddc_model::BeamSpec {
            zjust: ddc_model::Just::Pos,
            ..spec
        };
        let (p2, _) = beam_bar(&low, 0.0, 0.2, 0.4);
        let top = p2.iter().map(|q| q.z).fold(f32::MIN, f32::max);
        assert!((top - 3.0).abs() < 1e-4, "line at the top face");
    }
}
