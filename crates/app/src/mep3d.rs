// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Networks and components in 3D and partial scene rebuilds. The log of changed model rows
//! (`changes_since`) decides what to rebuild after an operation: architecture (walls, openings,
//! curtain walls, slabs) only if its tables changed; networks only the affected systems
//! (elbows, tees and transitions are recomputed from the system network); components by instance
//! (one mesh per type). Element numbers of architecture, networks and components are stable (by id,
//! not by list position), in the upper range; unchanged meshes are the same `Arc`s, so GPU chunks
//! and the scene index recompute only what was touched.

use crate::editor::levels::OTHER_LEVEL;
use crate::editor::{Editor, OpFn, Sel};
use crate::ui_kit;
use ddc_geom::{v2, V2};
use ddc_model::{Lod, MepDomain, MepPart, MepShape, Model, PipeMaterial, RunInfo, RunSpec};
use ddc_render::glam::{Mat4, Vec3, Vec4};
use ddc_render::scene::mesh_box;
use ddc_render::{tag_rgb, Aabb, Inst, Instanced, Scene, SceneMesh};
use ddc_ui::Lang;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use web_time::Instant;

use crate::scene_ids::SPAN;
pub use crate::scene_ids::{ARCH_BASE, COMP_BASE, MEP_BASE};
const COMP_RGB: [u8; 3] = [172, 164, 150];

/// Tables whose edits do not change the architectural part of the 3D scene.
const NON_ARCH: &[&str] = &[
    "mep_run",
    "mep_system",
    "mep_port",
    "comp",
    "comp_qty",
    "comp_type",
    "comp_family",
    "room",
    "room_qty",
    "room_sep",
    "roof_qty",
    "ceiling_qty",
    "railing_qty",
    "stair_qty",
    "member_qty",
    "wall_qty",
    "slab_qty",
    "dim",
    "dim_geo",
    "view_def",
    "grid",
    "ref_plane",
    "constraint_rule",
    "elec_circuit",
    "elec_circuit_dev",
    "elec_cable",
    // phase bookkeeping (u7) written by every op
    "ddc_dirty",
    "elem_phase",
];
const COMP_TABLES: [&str; 4] = ["comp", "comp_qty", "comp_type", "comp_family"];
/// Colour of the circuit cables in 3D: apart from the trays they lie in.
const CABLE_RGB: [u8; 3] = [230, 30, 30];

/// Stable numbers: row id to slot.
#[derive(Debug, Default)]
struct Slots {
    of: HashMap<i64, u32>,
    ids: Vec<i64>,
}

impl Slots {
    fn get(&mut self, id: i64) -> u32 {
        if let Some(&s) = self.of.get(&id) {
            return s;
        }
        let s = self.ids.len() as u32;
        self.ids.push(id);
        self.of.insert(id, s);
        s
    }
}

/// Stable architecture numbers: an element (a curtain wall together with its panels) to a slot block.
#[derive(Debug, Default)]
struct ArchSlots {
    of: HashMap<Sel, (u32, u32)>,
    /// Slot to (element, offset in the block).
    back: Vec<(Sel, u32)>,
}

impl ArchSlots {
    /// Start of a block of length `len`; a different length (panel count) gives a new block.
    fn block(&mut self, s: Sel, len: u32) -> u32 {
        if let Some(&(a, n)) = self.of.get(&s) {
            if n == len {
                return a;
            }
        }
        let a = self.back.len() as u32;
        self.back.extend((0..len).map(|k| (s, k)));
        self.of.insert(s, (a, len));
        a
    }
}

/// Panel and message strings by catalog key (one catalog per process).
fn tr(lang: Lang, key: &str) -> String {
    static CAT: OnceLock<ddc_ui::Catalog> = OnceLock::new();
    CAT.get_or_init(|| ddc_ui::Catalog::new(Lang::En))
        .t_in(lang, key)
}

/// Time of the last rebuild, ms, and what was taken from the cache.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Timing {
    pub arch_ms: f64,
    pub mep_ms: f64,
    pub comp_ms: f64,
    pub total_ms: f64,
    pub arch_reused: bool,
    /// Architecture meshes kept as before during its rebuild.
    pub arch_meshes_kept: usize,
    pub systems_rebuilt: usize,
    pub comps_reused: bool,
}

/// A run axis in plan for box selection and grips.
#[derive(Clone, Debug)]
struct Axis {
    level: i64,
    points: Vec<(f64, f64, f64)>,
    half: f64,
}

#[derive(Debug, Default)]
pub struct Big {
    head: Option<u64>,
    key: Option<(i64, u64)>,
    /// Architecture with stable numbers and the bounds of its meshes.
    arch: Vec<Arc<SceneMesh>>,
    arch_boxes: Vec<Aabb>,
    info: HashMap<i64, RunInfo>,
    mep: HashMap<i64, (Vec<Arc<SceneMesh>>, Vec<Aabb>)>,
    axes: HashMap<i64, Axis>,
    comps: Vec<Instanced>,
    comp_at: HashMap<i64, V2>,
    /// Circuit cables (`elec_cable`), drawn unless the 3D view hides their category.
    cables: (Vec<Arc<SceneMesh>>, Vec<Aabb>),
    runs: Slots,
    cslots: Slots,
    aslots: ArchSlots,
    /// Panel and message language (set by the shell; None means Russian).
    pub lang: Option<Lang>,
    plan: Mutex<Option<((u64, usize), Arc<crate::spatial::Plan2>)>>,
    /// How many times the plan index was rebuilt.
    pub plan_builds: std::sync::atomic::AtomicUsize,
    /// Property field buffers of the selected run.
    pub(crate) edit: Option<(i64, [String; 5])>,
    pub timing: Timing,
    /// Meshes the scene draws as cache instances (`edit3d::mesh_inst`): the provider skips them.
    pub instanced_meshes: std::collections::HashSet<i64>,
    /// Plants at fine detail (the realistic style; set by the shell).
    pub plants_fine: bool,
    /// Plant detail the components were last built with.
    comp_plants: Option<Lod>,
}

impl Big {
    fn lang(&self) -> Lang {
        self.lang.unwrap_or(Lang::Ru)
    }

    /// Pick scene elements (`scene` scenes); None means the number is not from here.
    pub fn range(&self, s: Sel) -> Option<(u32, u32)> {
        let e = match s {
            Sel::Mep(id) => MEP_BASE + *self.runs.of.get(&id)?,
            Sel::Comp(id) => COMP_BASE + *self.cslots.of.get(&id)?,
            Sel::Curtain(_) => {
                let (a, n) = *self.aslots.of.get(&s)?;
                return Some((ARCH_BASE + a, ARCH_BASE + a + n));
            }
            Sel::Panel(id, k) => {
                let (a, n) = *self.aslots.of.get(&Sel::Curtain(id))?;
                let e = ARCH_BASE + a + 1 + k;
                return (k + 1 < n).then_some((e, e + 1));
            }
            Sel::Wall(_) | Sel::Opening(_) | Sel::Slab(_) => ARCH_BASE + self.aslots.of.get(&s)?.0,
            _ => return None,
        };
        Some((e, e + 1))
    }

    pub fn sel_of(&self, e: u32) -> Option<Sel> {
        match e {
            _ if (ARCH_BASE..ARCH_BASE + SPAN).contains(&e) => {
                let &(s, k) = self.aslots.back.get((e - ARCH_BASE) as usize)?;
                Some(match s {
                    Sel::Curtain(id) if k > 0 => Sel::Panel(id, k - 1),
                    x => x,
                })
            }
            _ if (MEP_BASE..MEP_BASE + SPAN).contains(&e) => self
                .runs
                .ids
                .get((e - MEP_BASE) as usize)
                .map(|&id| Sel::Mep(id)),
            _ if (COMP_BASE..COMP_BASE + SPAN).contains(&e) => self
                .cslots
                .ids
                .get((e - COMP_BASE) as usize)
                .map(|&id| Sel::Comp(id)),
            _ => None,
        }
    }

    /// Plan outlines: a run is strips along axis segments, a component is a 0.3 m square.
    pub fn outlines(&self, s: Sel) -> Vec<Vec<V2>> {
        match s {
            Sel::Mep(id) => self.axes.get(&id).map_or(vec![], |a| {
                a.points
                    .windows(2)
                    .map(|w| band(v2(w[0].0, w[0].1), v2(w[1].0, w[1].1), a.half))
                    .collect()
            }),
            Sel::Comp(id) => self.comp_at.get(&id).map_or(vec![], |&p| {
                let h = 0.15;
                vec![vec![
                    p + v2(-h, -h),
                    p + v2(h, -h),
                    p + v2(h, h),
                    p + v2(-h, h),
                ]]
            }),
            _ => vec![],
        }
    }

    /// Level runs and components (for box selection, isolation, Tab).
    pub fn plan_sels(&self, level: i64) -> Vec<Sel> {
        let mut runs: Vec<i64> = self
            .axes
            .iter()
            .filter(|(_, a)| a.level == level)
            .map(|(id, _)| *id)
            .collect();
        runs.sort_unstable();
        let mut comps: Vec<i64> = self.comp_at.keys().copied().collect();
        comps.sort_unstable();
        runs.into_iter()
            .map(Sel::Mep)
            .chain(comps.into_iter().map(Sel::Comp))
            .collect()
    }

    /// The plan index over all view elements; rebuilt on a new revision or composition.
    pub fn plan_index(&self, ed: &Editor) -> Arc<crate::spatial::Plan2> {
        // key from list lengths: building the full element list per frame is itself O(N)
        let lens = [
            ed.walls.len(),
            ed.openings.len(),
            ed.curtains.len(),
            ed.slabs.len(),
            ed.rooms.len(),
            ed.mass_plan.len(),
            ed.bodies.len(),
            self.axes.len(),
            self.comp_at.len(),
        ];
        let n = lens
            .iter()
            .fold(ed.level as usize, |a, &n| a.wrapping_mul(1_000_003).wrapping_add(n));
        let key = (ed.revision, n);
        let mut g = self.plan.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, ix)) = g.as_ref() {
            if *k == key {
                return ix.clone();
            }
        }
        self.plan_builds.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let all = crate::selection::all(ed);
        let walls: HashMap<i64, &Vec<Vec<V2>>> = ed
            .walls
            .iter()
            .zip(&ed.plan)
            .map(|(w, o)| (w.id, o))
            .collect();
        let ix = Arc::new(crate::spatial::Plan2::build(all.into_iter().map(|s| {
            let pts = match s {
                Sel::Wall(id) => walls
                    .get(&id)
                    .map_or(vec![], |o| o.iter().flatten().copied().collect()),
                _ => crate::selection::outlines(ed, s)
                    .into_iter()
                    .flatten()
                    .collect(),
            };
            (s, pts)
        })));
        *g = Some((key, ix.clone()));
        ix
    }

    /// Walls visible in the plan rectangle; None means the model is small and culling is not needed.
    pub fn plan_visible(&self, ed: &Editor, a: V2, b: V2) -> Option<HashSet<i64>> {
        if ed.walls.len() < 2000 {
            return None;
        }
        let ix = self.plan_index(ed);
        Some(
            ix.rect(a, b, 0.5)
                .into_iter()
                .filter_map(|s| match s {
                    Sel::Wall(id) => Some(id),
                    _ => None,
                })
                .collect(),
        )
    }
}

fn band(a: V2, b: V2, half: f64) -> Vec<V2> {
    let d = b - a;
    let l = d.len().max(1e-12);
    let n = v2(-d.y / l * half, d.x / l * half);
    vec![a + n, b + n, b - n, a - n]
}

fn run_half(s: &RunSpec) -> f64 {
    let (w, h) = s.section.envelope().wh();
    w.max(h) / 2.0
}

/// A stable number for editor scene number `e` (position in its lists): the element block
/// plus an offset (a curtain wall panel). None means not a level element (other levels, nodes).
/// Called with an empty `ed.big`: editor numbers follow the lists.
fn arch_slot(ed: &Editor, slots: &mut ArchSlots, e: u32) -> Option<u32> {
    let ent = match ed.sel_of_element(e)? {
        // masses keep their own numbers (face = element, see `mass_tools::MASS_BASE`)
        Sel::Mass(_) | Sel::Anno(_) => return None,
        Sel::Panel(id, _) => Sel::Curtain(id),
        x => x,
    };
    let (a, b) = match ent {
        Sel::Wall(_) | Sel::Opening(_) => (e, e + 1),
        _ => ed.element_range(ent)?,
    };
    let k = e.checked_sub(a).filter(|&k| k < b - a)?;
    Some(ARCH_BASE + slots.block(ent, b - a) + k)
}

/// Architecture with stable numbers; a mesh matching the previous one (number, layer, vertices,
/// triangles) is kept: the same `Arc` and bounds. Also returns the count of such meshes.
fn stable_arch(
    ed: &Editor,
    raw: Vec<Arc<SceneMesh>>,
    slots: &mut ArchSlots,
    old: &[Arc<SceneMesh>],
    old_boxes: &[Aabb],
) -> (Vec<Arc<SceneMesh>>, Vec<Aabb>, usize) {
    type Key = (u32, u32, usize, usize, [u32; 3]);
    let key = |m: &SceneMesh, el: u32| -> Key {
        let p = m.positions.first().map_or([0; 3], |p| p.map(f32::to_bits));
        (el, m.tag, m.positions.len(), m.triangles.len(), p)
    };
    let mut by: HashMap<Key, Vec<usize>> = HashMap::new();
    for (i, m) in old.iter().enumerate() {
        by.entry(key(&**m, m.element)).or_default().push(i);
    }
    let mut nums: HashMap<u32, Option<u32>> = HashMap::new();
    let (mut out, mut boxes) = (Vec::with_capacity(raw.len()), Vec::with_capacity(raw.len()));
    let mut kept = 0;
    for m in raw {
        let el = *nums
            .entry(m.element)
            .or_insert_with(|| arch_slot(ed, slots, m.element));
        let el = el.unwrap_or(m.element);
        let same = by.get_mut(&key(&*m, el)).and_then(|v| {
            let at = v.iter().position(|&i| {
                old[i].positions == m.positions && old[i].triangles == m.triangles
            })?;
            Some(v.swap_remove(at))
        });
        match same {
            Some(i) => {
                out.push(old[i].clone());
                boxes.push(old_boxes[i]);
                kept += 1;
            }
            None => {
                let mut m = Arc::try_unwrap(m).unwrap_or_else(|a| (*a).clone());
                m.element = el;
                boxes.push(mesh_box(&m));
                out.push(Arc::new(m));
            }
        }
    }
    (out, boxes, kept)
}

fn to_scene(mesh: &ddc_geom::mesh::Mesh, element: u32, tag: u32, dz: f64) -> SceneMesh {
    SceneMesh {
        element,
        tag,
        positions: mesh
            .positions
            .iter()
            .map(|p| [p.x as f32, p.y as f32, (p.z - dz) as f32])
            .collect(),
        triangles: mesh.triangles.clone(),
    }
}

/// 3D scene: architecture (from the cache if its tables did not change), networks by system,
/// components as instances. Replaces `scene_all` in the 3D view.
pub fn scene(ed: &mut Editor) -> Scene {
    let t0 = Instant::now();
    let mut big = std::mem::take(&mut ed.big);
    let ch = match big.head {
        Some(h) => ed.model.changes_since(h),
        None => ddc_model::Changes {
            all: true,
            ..Default::default()
        },
    };
    let key = (ed.level, ed.elevation.to_bits());
    let fresh = ch.all || big.key != Some(key);
    let mut tm = Timing::default();

    let t = Instant::now();
    if fresh || ch.rows.keys().any(|t| !NON_ARCH.contains(&t.as_str())) {
        if big.key != Some(key) || big.aslots.back.len() > (SPAN / 2) as usize {
            big.aslots = ArchSlots::default();
        }
        let span = ddc_model::perf::span("compose.arch");
        let raw = ed.scene_all().meshes;
        drop(span);
        let (arch, boxes, kept) = stable_arch(ed, raw, &mut big.aslots, &big.arch, &big.arch_boxes);
        (big.arch, big.arch_boxes, tm.arch_meshes_kept) = (arch, boxes, kept);
        // texture frames of the walls follow the stable numbers just given
        let slots = &big.aslots;
        crate::mat_anchor::publish(ed, |id| {
            slots
                .of
                .get(&Sel::Wall(id))
                .map(|&(a, _)| ARCH_BASE + a)
        });
    } else {
        tm.arch_reused = true;
    }
    tm.arch_ms = ms(t);

    let t = Instant::now();
    let mep_all = fresh || ch.touched("mep_system") || ch.touched("level") || ch.whole("mep_run");
    if mep_all || ch.touched("mep_run") {
        let info = ed.model.mep_run_info().unwrap_or_default();
        let dirty: HashSet<i64> = if mep_all {
            big.mep
                .keys()
                .copied()
                .chain(info.values().map(|r| r.system))
                .collect()
        } else {
            ch.ids("mep_run")
                .flat_map(|id| [big.info.get(&id), info.get(&id)])
                .flatten()
                .map(|r| r.system)
                .collect()
        };
        let runs: Vec<i64> = if mep_all {
            big.axes.clear();
            info.keys().copied().collect()
        } else {
            ch.ids("mep_run").collect()
        };
        for id in runs {
            match ed.model.mep_run(id) {
                Ok(s) => {
                    let half = run_half(&s);
                    big.axes.insert(
                        id,
                        Axis {
                            level: s.level,
                            points: s.points,
                            half,
                        },
                    );
                }
                Err(_) => {
                    big.axes.remove(&id);
                }
            }
        }
        let (level, dz) = (ed.level, ed.elevation);
        for sys in &dirty {
            big.mep.remove(sys);
            let bodies = ed.model.mep_bodies(Some(*sys)).unwrap_or_default();
            let mut out = Vec::with_capacity(bodies.len());
            for b in bodies {
                let Some(ri) = info.get(&b.run) else { continue };
                let element = if ri.level == level {
                    MEP_BASE + big.runs.get(b.run)
                } else {
                    OTHER_LEVEL
                };
                let shell = matches!(b.part, MepPart::Insulation | MepPart::Lining);
                out.push(Arc::new(to_scene(
                    &b.mesh,
                    element,
                    tag_rgb(ri.color, shell),
                    dz,
                )));
            }
            if !out.is_empty() {
                let boxes = out.iter().map(|m| mesh_box(m)).collect();
                big.mep.insert(*sys, (out, boxes));
            }
        }
        tm.systems_rebuilt = dirty.len();
        big.info = info;
    }
    tm.mep_ms = ms(t);

    let t = Instant::now();
    let plants = plant_lod(&ed.model, big.plants_fine);
    if fresh
        || big.comp_plants != Some(plants)
        || COMP_TABLES.iter().any(|t| ch.touched(t))
        || ch.touched("view_gfx")
        || ch.touched("view_template")
    {
        big.comp_plants = Some(plants);
        big.comps = comp_groups(
            &ed.model,
            ed.level,
            ed.elevation,
            plants,
            &mut big.cslots,
            &mut big.comp_at,
        );
    } else {
        tm.comps_reused = true;
    }
    tm.comp_ms = ms(t);

    if fresh
        || ["elec_cable", "elec_circuit", "view_gfx", "view_template"].iter().any(|t| ch.touched(t))
    {
        big.cables = cable_meshes(&ed.model, ed.elevation);
    }

    // the scene is made of shared meshes: pointers and bounds are copied, not vertices
    let mut systems: Vec<&i64> = big.mep.keys().collect();
    systems.sort_unstable();
    let n: usize = big.arch.len() + systems.iter().map(|s| big.mep[*s].0.len()).sum::<usize>();
    let (mut meshes, mut boxes) = (Vec::with_capacity(n), Vec::with_capacity(n));
    meshes.extend(big.arch.iter().cloned());
    boxes.extend_from_slice(&big.arch_boxes);
    meshes.extend(big.cables.0.iter().cloned());
    boxes.extend_from_slice(&big.cables.1);
    for s in systems {
        let (m, b) = &big.mep[s];
        meshes.extend(m.iter().cloned());
        boxes.extend_from_slice(b);
    }
    let sc = Scene::from_parts(meshes, boxes).with_instanced(big.comps.clone());
    big.head = Some(ed.model.change_head());
    big.key = Some(key);
    tm.total_ms = ms(t0);
    big.timing = tm;
    ed.big = big;
    sc
}

/// Cable pieces of the circuits as scene bodies, not selectable; none when the 3D view hides
/// the `elec_cable` category.
pub fn cable_meshes(m: &Model, dz: f64) -> (Vec<Arc<SceneMesh>>, Vec<Aabb>) {
    let hidden = m
        .view_paint(ddc_model::ViewKey::ThreeD)
        .is_ok_and(|p| p.hidden_cats.iter().any(|c| c == "elec_cable"));
    if hidden {
        return (vec![], vec![]);
    }
    let mut out = vec![];
    for (_, t) in m.circuit_cables(None).unwrap_or_default() {
        let mesh = ddc_model::cable_mesh(&t.points);
        out.push(Arc::new(to_scene(&mesh, OTHER_LEVEL, tag_rgb(CABLE_RGB, false), dz)));
    }
    let boxes = out.iter().map(|m| mesh_box(m)).collect();
    (out, boxes)
}

/// Since `since` only networks (and derived quantities) changed: the editor lists
/// (walls, openings, rooms) need no re-read, a new revision is enough.
pub fn only_mep(m: &Model, since: u64) -> bool {
    let ch = m.changes_since(since);
    (ch.touched("mep_run") || ch.touched("mep_system"))
        && ch.only(&[
            "mep_run",
            "mep_system",
            "mep_port",
            "roof_qty",
            "room_qty",
            "ceiling_qty",
            "comp_qty",
            "railing_qty",
            "stair_qty",
            "member_qty",
            "ddc_dirty",
            "elem_phase",
        ])
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Plant detail of the 3D scene: fine in the realistic style (far plants still drop to their
/// coarser pictures by distance), the view's level otherwise.
pub fn plant_lod(m: &Model, fine: bool) -> Lod {
    if fine {
        Lod::Fine
    } else {
        crate::view_gfx_ui::lod_3d(m)
    }
}

/// Level components: a group per shared mesh, placements are matrices with elevation from the level.
fn comp_groups(
    m: &Model,
    level: i64,
    dz: f64,
    plants: Lod,
    slots: &mut Slots,
    at: &mut HashMap<i64, V2>,
) -> Vec<Instanced> {
    at.clear();
    // every level is drawn, as walls and runs are; only the current level's components are picked
    let Ok(cs) = m.component_scene_plants(crate::view_gfx_ui::lod_3d(m), plants, None) else {
        return vec![];
    };
    let here: HashSet<i64> = m
        .sql()
        .prepare("SELECT id FROM comp WHERE level=?1")
        .and_then(|mut st| st.query_map([level], |r| r.get(0))?.collect())
        .unwrap_or_default();
    let mut by: Vec<Vec<Inst>> = vec![vec![]; cs.meshes.len()];
    let mut fam: Vec<i64> = vec![0; cs.meshes.len()];
    // the coarser pictures' errors are for the unit mesh: the largest placement sizes them
    let mut big_scale: Vec<f64> = vec![1.0; cs.meshes.len()];
    for i in &cs.instances {
        if let Some(s) = big_scale.get_mut(i.mesh) {
            *s = s.max(i.scale);
        }
        let r = i.matrix();
        let f = |k: usize| r[k] as f32;
        let mtx = Mat4::from_cols(
            Vec4::new(f(0), f(4), f(8), 0.0),
            Vec4::new(f(1), f(5), f(9), 0.0),
            Vec4::new(f(2), f(6), f(10), 0.0),
            Vec4::new(f(3), f(7), (r[11] - dz) as f32, 1.0),
        );
        let element = if here.contains(&i.id) {
            at.insert(i.id, v2(r[3], r[7]));
            COMP_BASE + slots.get(i.id)
        } else {
            OTHER_LEVEL
        };
        if let Some(v) = by.get_mut(i.mesh) {
            v.push(Inst { m: mtx, element });
            fam[i.mesh] = i.family;
        }
    }
    let mut out = vec![];
    let mut colours: HashMap<String, [u8; 3]> = HashMap::new();
    for (k, v) in by.into_iter().enumerate() {
        if v.is_empty() {
            continue;
        }
        let mut rgb_of = |n: &str| comp_rgb(m, n, &mut colours);
        let groups = comp_meshes(&cs.meshes[k], &cs.materials[k], &mut rgb_of);
        let lods: Vec<Vec<SceneMesh>> = cs.lods[k].iter().map(|(x, mats, _)| comp_meshes(x, mats, &mut rgb_of)).collect();
        for (g, proto) in groups.into_iter().enumerate() {
            let tag = proto.tag;
            // coarser pictures of a plant, one per colour group
            let levels: Vec<(Arc<SceneMesh>, f32)> = lods
                .iter()
                .zip(&cs.lods[k])
                .filter_map(|(ms, l)| ms.iter().find(|x| x.tag == tag).cloned().map(|x| (Arc::new(x), (l.2 * big_scale[k]) as f32)))
                .collect();
            let key = (fam[k] as u64) << 40 | (k as u64) << 8 | g as u64;
            out.push(Instanced::new(key, Arc::new(proto), v.clone()).with_lods(levels));
        }
    }
    out
}

/// Display colour of a component material: the colour of its row in the material table (a
/// planting material has a default one); the common component grey when it has none.
fn comp_rgb(m: &Model, material: &str, cache: &mut HashMap<String, [u8; 3]>) -> [u8; 3] {
    if let Some(c) = cache.get(material) {
        return *c;
    }
    let rgb = if material.is_empty() {
        COMP_RGB
    } else {
        m.material(material)
            .ok()
            .and_then(|x| x.spec.color)
            .map_or(COMP_RGB, |c| [(c >> 16) as u8, (c >> 8) as u8, c as u8])
    };
    cache.insert(material.to_string(), rgb);
    rgb
}

/// One scene mesh per display colour of the parts of a component body.
fn comp_meshes(mesh: &ddc_geom::mesh::Mesh, mats: &[String], rgb_of: &mut dyn FnMut(&str) -> [u8; 3]) -> Vec<SceneMesh> {
    let mut by: std::collections::BTreeMap<[u8; 3], Vec<usize>> = Default::default();
    for i in 0..mesh.triangles.len() {
        let part = mesh.face_of.get(i).copied().unwrap_or(0) as usize;
        by.entry(mats.get(part).map_or(COMP_RGB, |m| rgb_of(m))).or_default().push(i);
    }
    if by.len() <= 1 {
        let rgb = by.keys().next().copied().unwrap_or(COMP_RGB);
        return vec![to_scene(mesh, 0, tag_rgb(rgb, false), 0.0)];
    }
    by.into_iter()
        .map(|(rgb, tris)| {
            let mut map: HashMap<u32, u32> = HashMap::new();
            let (mut positions, mut triangles) = (vec![], vec![]);
            for i in tris {
                let t = mesh.triangles[i];
                let mut o = [0u32; 3];
                for j in 0..3 {
                    o[j] = match map.get(&t[j]) {
                        Some(&id) => id,
                        None => {
                            let p = mesh.positions[t[j] as usize];
                            positions.push([p.x as f32, p.y as f32, p.z as f32]);
                            let id = positions.len() as u32 - 1;
                            map.insert(t[j], id);
                            id
                        }
                    };
                }
                triangles.push(o);
            }
            SceneMesh { element: 0, tag: tag_rgb(rgb, false), positions, triangles }
        })
        .collect()
}

/// Run section fill color in a section view ("mep#rrggbb" is the system color, lighter).
pub fn cut_fill(material: &str) -> Option<egui::Color32> {
    let h = material.strip_prefix("mep#")?;
    let v = u32::from_str_radix(h, 16).ok()?;
    let c = |s: u32| ((v >> s) & 255) as u8;
    let mix = |x: u8| (x as f32 * 0.75 + 255.0 * 0.25) as u8;
    Some(egui::Color32::from_rgb(mix(c(16)), mix(c(8)), mix(c(0))))
}

pub(crate) fn domain_key(d: MepDomain) -> &'static str {
    match d {
        MepDomain::Duct => "mep3d.duct",
        MepDomain::Pipe => "mep3d.pipe",
        MepDomain::CableTray => "mep3d.tray",
        MepDomain::Conduit => "mep3d.conduit",
    }
}

/// Selected run properties: system, section, insulation; a size edit makes a new run
/// on the same axis, network fittings are recomputed on rebuild. false means no run is selected.
pub fn props(ui: &mut egui::Ui, ed: &mut Editor) -> bool {
    let Some(id) = ed.selection.iter().find_map(|s| match s {
        Sel::Mep(i) => Some(*i),
        _ => None,
    }) else {
        return false;
    };
    let Ok(spec) = ed.model.mep_run(id) else {
        return false;
    };
    let u = ed.tool.units;
    let lang = ed.big.lang();
    let t = |k: &str| tr(lang, k);
    let th = ddc_ui::theme::of(ui.ctx());
    let dec = crate::modify_opts::number_format(lang == Lang::Ru).decimal;
    let rejected = t(REJECTED);
    let mut reset = false;
    let lvl_z = ed
        .model
        .level_list()
        .unwrap_or_default()
        .into_iter()
        .find(|l| l.0 == spec.level)
        .map_or(0.0, |l| l.2);
    let (w, h) = spec.section.shape.wh();
    let round = matches!(spec.section.shape, MepShape::Round { .. });
    if ed.big.edit.as_ref().is_none_or(|e| e.0 != id) {
        let f = |x: f64| crate::units::edit_length(x, u);
        let rel = ddc_model::start_z(&spec) - lvl_z;
        let pct = ddc_ui::widgets::field::format_number(
            ddc_model::run_slope_pct(&spec),
            2,
            crate::modify_opts::number_format(lang == Lang::Ru),
        );
        ed.big.edit = Some((
            id,
            [f(w), f(h), f(spec.section.insulation), f(rel), pct],
        ));
    }
    let systems: Vec<ddc_model::MepSystem> = ed
        .model
        .mep_systems()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.kind.domains().contains(&spec.section.domain))
        .collect();
    let mut commit: Option<RunSpec> = None;
    let mut refused: Option<String> = None;
    let icon = match spec.section.domain {
        MepDomain::Duct => "duct",
        MepDomain::Pipe => "pipe",
        MepDomain::CableTray => "tray",
        MepDomain::Conduit => "conduit",
    };
    ui_kit::element_header(ui, icon, &t(domain_key(spec.section.domain)), "", id);
    {
        let Some((_, buf)) = ed.big.edit.as_mut() else {
            return true;
        };
        ui_kit::props(ui, "mep_run", |ui| {
            ui_kit::kv_widget(ui, &t("mep3d.system"), |ui| {
                let paper = ui.visuals().panel_fill;
                let missing = crate::tools::mep_missing_label(lang == Lang::Ru, spec.system);
                let rows = run_system_rows(&systems, spec.system, &missing);
                let salt = format!("mep_run_system_{id}");
                let mut cur = Some(spec.system);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let changed = crate::tools::mep_system_dropdown_w(
                        ui,
                        &th,
                        &salt,
                        &mut cur,
                        &rows,
                        paper,
                        ui_kit::WIDGET_W,
                    );
                    if let (true, Some(sid)) = (changed, cur) {
                        commit = Some(RunSpec {
                            system: sid,
                            ..spec.clone()
                        });
                    }
                });
            });
            let len: f64 = spec
                .points
                .windows(2)
                .map(|p| {
                    let d = (p[1].0 - p[0].0, p[1].1 - p[0].1, p[1].2 - p[0].2);
                    (d.0 * d.0 + d.1 * d.1 + d.2 * d.2).sqrt()
                })
                .sum();
            ui_kit::kv_num(ui, &t("mep3d.length"), crate::units::edit_length(len, u));
            let tip = &t("mep3d.tip");
            let [bw, bh, bi, bz, bs] = buf;
            if let Some(x) = ui_kit::kv_length(ui, &t("mep3d.elevation"), bz, u, tip) {
                pick(ddc_model::with_elevation(&spec, lvl_z + x), &mut commit, &mut refused, &rejected);
            }
            if spec.section.domain == MepDomain::Pipe {
                let slope_err = t(SLOPE_ERR);
                let bad = !bs.trim().is_empty() && slope_parse(bs, dec).is_err();
                let cap = t("mep3d.slope");
                let out = ui_kit::kv_widget(ui, &cap, |ui| {
                    ddc_ui::widgets::TextField::new(bs)
                        .labelled_by(egui::Id::new(("mep_run_slope.lbl", id)), &cap)
                        .id_salt(("mep_run_slope", id))
                        .right_aligned()
                        .unit("%")
                        .width(ui_kit::WIDGET_W)
                        .error(bad)
                        .error_text(&slope_err)
                        .show(ui, &th)
                });
                if bad {
                    out.response.on_hover_text(&slope_err);
                }
                if out.committed {
                    match slope_parse(bs, dec) {
                        Ok(p) => pick(ddc_model::with_run_slope(&spec, p), &mut commit, &mut refused, &rejected),
                        Err(k) => refused = Some(t(k)),
                    }
                } else if out.cancelled {
                    reset = true;
                }
                let mat = spec.material.unwrap_or(PipeMaterial::Steel);
                let mats: Vec<String> = PipeMaterial::ALL
                    .iter()
                    .map(|m| t(&format!("mep.mat.{}", m.name())))
                    .collect();
                let mat_ix = PipeMaterial::ALL.iter().position(|m| *m == mat).unwrap_or(0);
                if let Some(i) = ui_kit::kv_widget(ui, &t("mep.material"), |ui| {
                    choice(ui, &th, ("mep_run_mat", id), mat_ix, &mats)
                }) {
                    pick(
                        ddc_model::with_material(&spec, PipeMaterial::ALL[i]),
                        &mut commit,
                        &mut refused,
                        &rejected,
                    );
                }
                let mut dns: Vec<u32> = mat.sizes().iter().map(|z| z.dn).collect();
                if let Some(d) = spec.dn.filter(|d| !dns.contains(d)) {
                    dns.push(d);
                    dns.sort_unstable();
                }
                let (dn_labels, dn_sel, dn_off) = dn_items(&dns, spec.dn, u, cat(lang));
                if let Some(i) = ui_kit::kv_widget(ui, &t("mep_units.dn"), |ui| {
                    choice(ui, &th, ("mep_run_dn", id), dn_sel, &dn_labels)
                }) {
                    if let Some(&dn) = i.checked_sub(dn_off).and_then(|k| dns.get(k)) {
                        pick(ddc_model::with_dn(&spec, dn), &mut commit, &mut refused, &rejected);
                    }
                }
            }
            if spec.section.domain == MepDomain::Duct {
                let cur = ddc_model::DuctShape::of(spec.section.shape);
                let shapes: Vec<String> = ddc_model::DuctShape::ALL
                    .iter()
                    .map(|d| t(&format!("mep3d.shape.{}", d.key())))
                    .collect();
                let cur_ix = ddc_model::DuctShape::ALL.iter().position(|d| *d == cur).unwrap_or(0);
                if let Some(i) = ui_kit::kv_widget(ui, &t("mep3d.shape"), |ui| {
                    choice(ui, &th, ("mep_run_shape", id), cur_ix, &shapes)
                }) {
                    pick(
                        ddc_model::with_duct_shape(&spec, ddc_model::DuctShape::ALL[i]),
                        &mut commit,
                        &mut refused,
                        &rejected,
                    );
                }
            }
            let mut s = spec.clone();
            s.slope = 0.0;
            if round {
                if let Some(d) = ui_kit::kv_length(ui, &t("mep3d.diameter"), bw, u, tip) {
                    s.section.shape = MepShape::Round { d };
                    s.dn = None;
                    commit = Some(s.clone());
                }
            } else {
                let mk = |w: f64, h: f64| match spec.section.shape {
                    MepShape::Oval { .. } => MepShape::Oval { w, h },
                    _ => MepShape::Rect { w, h },
                };
                if let Some(x) = ui_kit::kv_length(ui, &t("mep3d.width"), bw, u, tip) {
                    s.section.shape = mk(x, h);
                    commit = Some(s.clone());
                }
                if let Some(y) = ui_kit::kv_length(ui, &t("mep3d.height"), bh, u, tip) {
                    s.section.shape = mk(w, y);
                    commit = Some(s.clone());
                }
            }
            if let Some(x) = ui_kit::kv_length(ui, &t("mep3d.insulation"), bi, u, tip) {
                s.section.insulation = x.max(0.0);
                commit = Some(s);
            }
        });
        if reset {
            ed.big.edit = None;
        }
        crate::elem::mep::run_rows(ui, ed, id);
    }
    if let Some(e) = refused {
        ed.status = Some(t("mep3d.not_changed").replace("{e}", &e));
    }
    if let Some(s) = commit {
        let ok = s.section.shape.valid();
        let r = if ok {
            let s2 = s.clone();
            ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
                m.update_mep_run(id, &s2)
            })))
            .map_err(|_| rejected.clone())
        } else {
            Err(t("mep3d.size_positive"))
        };
        ed.big.edit = None;
        ed.status = Some(match r {
            Ok(()) => t("mep3d.changed"),
            Err(e) => t("mep3d.not_changed").replace("{e}", &e),
        });
    }
    true
}

fn pick(
    r: Result<RunSpec, ddc_model::Error>,
    commit: &mut Option<RunSpec>,
    err: &mut Option<String>,
    rejected: &str,
) {
    match r {
        Ok(s) => *commit = Some(s),
        Err(_) => *err = Some(rejected.to_owned()),
    }
}

/// System rows of the run panel. Row 0 of the tool bar ("no system") is dropped: a run always
/// has one. A system missing from the list keeps its own row (`missing`, warning mark) instead
/// of showing another.
pub(crate) fn run_system_rows(
    systems: &[ddc_model::MepSystem],
    cur: i64,
    missing: &str,
) -> Vec<crate::tools::MepSysRow> {
    let mut rows = crate::tools::mep_system_items(systems, "", crate::tools::MepMark::Dash);
    if systems.iter().any(|s| s.id == cur) {
        rows.remove(0);
    } else {
        rows[0].id = Some(cur);
        rows[0].label = missing.to_owned();
        rows[0].mark = crate::tools::MepMark::Missing;
    }
    rows
}

/// Catalog keys of the run panel messages.
const SLOPE_ERR: &str = "mep_dn.slope.err";
const REJECTED: &str = "mep_dn.err.rejected";

/// Slope typed in the run panel, percent: "1.5", "1,5 %", per mille "5\u{2030}", ratio "1:50".
/// `dec` is the decimal separator of the UI language; the other of `,` `.` is a group mark
/// when both occur. Errors are catalog keys, never a parser message.
pub(crate) fn slope_parse(text: &str, dec: char) -> Result<f64, &'static str> {
    let s: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let num = |p: &str| -> Option<f64> {
        let p = if p.contains(',') && p.contains('.') {
            let group = if dec == ',' { '.' } else { ',' };
            p.replace(group, "")
        } else {
            p.to_owned()
        };
        p.replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
    };
    let pct = if let Some((a, b)) = s.split_once(':') {
        match (num(a), num(b)) {
            (Some(a), Some(b)) if b != 0.0 => Some(a / b * 100.0),
            _ => None,
        }
    } else if let Some(p) = s.strip_suffix('\u{2030}') {
        num(p).map(|v| v / 10.0)
    } else {
        num(s.strip_suffix('%').unwrap_or(&s))
    };
    pct.filter(|p| p.abs() <= 100.0).ok_or(SLOPE_ERR)
}

/// Size text of a DN row in the project units, or the muted-wording placeholder when unset.
pub(crate) fn dn_text(dn: Option<u32>, units: crate::units::Units, cat: &ddc_ui::Catalog) -> String {
    match dn {
        Some(d) => crate::tools::mep_size_label(d, units, cat),
        None => cat.t("mep_dn.dn.unset"),
    }
}

/// Dropdown rows for `dns`: the texts, the selected row and the offset of the first size (1
/// when a leading placeholder stands for an unset DN).
pub(crate) fn dn_items(
    dns: &[u32],
    cur: Option<u32>,
    units: crate::units::Units,
    cat: &ddc_ui::Catalog,
) -> (Vec<String>, usize, usize) {
    let mut items: Vec<String> = dns.iter().map(|&d| dn_text(Some(d), units, cat)).collect();
    if cur.is_none() {
        items.insert(0, dn_text(None, units, cat));
        return (items, 0, 1);
    }
    let sel = dns.iter().position(|d| Some(*d) == cur).unwrap_or(0);
    (items, sel, 0)
}

/// One catalog per language for texts that need a catalog value (units, sizes).
fn cat(lang: Lang) -> &'static ddc_ui::Catalog {
    static CATS: OnceLock<Vec<ddc_ui::Catalog>> = OnceLock::new();
    let all = CATS.get_or_init(|| Lang::ALL.iter().map(|l| ddc_ui::Catalog::new(*l)).collect());
    &all[lang as usize]
}

/// Kit dropdown over `items`; the new index when the user picked another row.
fn choice(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    salt: impl std::hash::Hash,
    sel: usize,
    items: &[String],
) -> Option<usize> {
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    let mut s = sel;
    ddc_ui::widgets::field::dropdown(ui, th, salt, &mut s, &refs, Some(ui_kit::WIDGET_W));
    (s != sel).then_some(s)
}

/// 3D grips of the selected run: axis vertices and the midpoint (move) in scene coordinates.
pub fn grips(ed: &Editor) -> Vec<crate::grips3d::Grip> {
    use crate::grips3d::{Grip, GripKind};
    let [Sel::Mep(id)] = ed.selection.as_slice() else {
        return vec![];
    };
    let Some(a) = ed.big.axes.get(id) else {
        return vec![];
    };
    let dz = ed.elevation;
    let p = |q: &(f64, f64, f64)| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - dz) as f32);
    let mut v: Vec<Grip> = a
        .points
        .iter()
        .enumerate()
        .map(|(k, q)| Grip {
            at: p(q),
            kind: GripKind::Vertex(k),
        })
        .collect();
    if a.points.len() >= 2 {
        let (q0, q1) = (p(&a.points[0]), p(&a.points[1]));
        v.push(Grip {
            at: (q0 + q1) * 0.5,
            kind: GripKind::Move,
        });
    }
    v
}

/// Ends of the selected run in scene coordinates: a move snaps by them.
pub fn ends(ed: &Editor) -> Vec<Vec3> {
    let [Sel::Mep(id)] = ed.selection.as_slice() else {
        return vec![];
    };
    let dz = ed.elevation;
    ed.big.axes.get(id).map_or(vec![], |a| {
        [a.points.first(), a.points.last()]
            .into_iter()
            .flatten()
            .map(|q| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - dz) as f32))
            .collect()
    })
}

/// Grip snap points: nearby ends of other runs (by scene index); never its own.
pub fn snaps(ed: &Editor, index: Option<&crate::spatial::Index>, near: Vec3, r: f32) -> Vec<Vec3> {
    let dz = ed.elevation;
    let own = match ed.selection.as_slice() {
        [Sel::Mep(id)] => Some(*id),
        _ => None,
    };
    let ids: Vec<i64> = match index {
        Some(ix) => ix
            .elements(&ix.near(near, r))
            .into_iter()
            .filter_map(|e| match ed.big.sel_of(e) {
                Some(Sel::Mep(i)) => Some(i),
                _ => None,
            })
            .collect(),
        None => ed.big.axes.keys().copied().collect(),
    };
    ids.into_iter()
        .filter(|i| Some(*i) != own)
        .filter_map(|i| ed.big.axes.get(&i))
        .flat_map(|a| [a.points.first(), a.points.last()])
        .flatten()
        .map(|q| Vec3::new(q.0 as f32, q.1 as f32, (q.2 - dz) as f32))
        .filter(|q| q.distance(near) <= r)
        .collect()
}

/// The run axis after dragging grip `kind` from `from` to `to` (scene): the shift is computed in
/// f64 and added to the previous points, so far coordinates do not lose f32 precision.
pub fn dragged(s: &RunSpec, kind: crate::grips3d::GripKind, from: Vec3, to: Vec3) -> RunSpec {
    use crate::grips3d::GripKind;
    let d = (
        to.x as f64 - from.x as f64,
        to.y as f64 - from.y as f64,
        to.z as f64 - from.z as f64,
    );
    let mut s = s.clone();
    let sh = |p: &mut (f64, f64, f64)| *p = (p.0 + d.0, p.1 + d.1, p.2 + d.2);
    match kind {
        GripKind::Vertex(k) => {
            if let Some(p) = s.points.get_mut(k) {
                sh(p);
            }
        }
        GripKind::Move => s.points.iter_mut().for_each(sh),
    }
    s
}

/// Apply a 3D grip drag: a vertex, a move or a copy (Alt) of the run.
pub fn apply_grip(ed: &mut Editor, d: crate::grips3d::Done) {
    use crate::grips3d::GripKind;
    let [Sel::Mep(id)] = ed.selection.as_slice() else {
        return;
    };
    let id = *id;
    let Ok(s) = ed.model.mep_run(id) else {
        return;
    };
    if (d.to - d.from).length() < 1e-4 {
        return;
    }
    if let GripKind::Vertex(k) = d.kind {
        if k >= s.points.len() {
            return;
        }
    }
    let s = dragged(&s, d.kind, d.from, d.to);
    ed.status = None;
    let r = if d.copy && d.kind == GripKind::Move {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.add_mep_run(&s).map(|_| ())
        })))
    } else {
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.mep_edit_run(id, &s)
        })))
    };
    let lang = ed.big.lang();
    ed.status = Some(match r {
        Ok(()) if d.copy => tr(lang, "mep3d.copied"),
        // a follow that left the network with problems already wrote its note
        Ok(()) => ed.status.take().unwrap_or_else(|| tr(lang, "mep3d.changed")),
        Err(e) => tr(lang, "mep3d.not_changed").replace("{e}", &crate::elem::mep::explain(&e)),
    });
}

/// Demo network: a room, a supply duct with insulation and an elbow,
/// a tee branch, heating pipes and a cable tray; a duct run is selected.
pub fn demo(ed: &mut Editor) -> Result<(), String> {
    use ddc_model::{MepSection, SystemKind};
    let e = |e: ddc_model::Error| e.to_string();
    let level = ed.level;
    let z = ed.elevation;
    let wt = ed
        .types
        .first()
        .map(|t| t.id)
        .ok_or_else(|| tr(ed.big.lang(), "mep3d.no_wall_type"))?;
    for (a, b) in [
        ((0.0, 0.0), (12.0, 0.0)),
        ((12.0, 0.0), (12.0, 8.0)),
        ((12.0, 8.0), (0.0, 8.0)),
        ((0.0, 8.0), (0.0, 0.0)),
    ] {
        ed.model
            .add_wall(&ddc_model::WallSpec {
                a,
                b,
                level,
                base_offset: 0.0,
                height: 3.6,
                wall_type: wt,
                offset0: None,
            })
            .map_err(e)?;
    }
    let lang = ed.big.lang();
    let m = &mut ed.model;
    let sa = m
        .add_mep_system(&tr(lang, "mep3d.demo.supply"), SystemKind::SupplyAir)
        .map_err(e)?;
    let ht = m
        .add_mep_system(&tr(lang, "mep3d.demo.heating"), SystemKind::Heating)
        .map_err(e)?;
    let pw = m
        .add_mep_system(&tr(lang, "mep3d.demo.power"), SystemKind::Power)
        .map_err(e)?;
    let mut duct = MepSection::new(MepDomain::Duct, MepShape::Rect { w: 0.5, h: 0.3 });
    duct.wall = 0.001;
    duct.insulation = 0.04;
    let main = m
        .add_mep_run(&RunSpec::new(
            sa,
            level,
            duct,
            vec![
                (1.0, 2.0, z + 3.0),
                (9.0, 2.0, z + 3.0),
                (9.0, 6.5, z + 3.0),
            ],
        ))
        .map_err(e)?;
    let mut br = MepSection::new(MepDomain::Duct, MepShape::Round { d: 0.25 });
    br.wall = 0.001;
    m.add_mep_run(&RunSpec::new(
        sa,
        level,
        br,
        vec![
            (4.0, 2.0, z + 3.0),
            (4.0, 6.0, z + 3.0),
            (4.0, 6.0, z + 2.4),
        ],
    ))
    .map_err(e)?;
    for (y, d) in [(7.2, 0.05), (7.45, 0.04)] {
        let mut p = MepSection::new(MepDomain::Pipe, MepShape::Round { d });
        p.wall = 0.003;
        p.insulation = 0.02;
        m.add_mep_run(&RunSpec::new(
            ht,
            level,
            p,
            vec![(0.6, y, z + 0.4), (10.5, y, z + 0.4), (10.5, y, z + 2.2)],
        ))
        .map_err(e)?;
    }
    m.add_mep_run(&RunSpec::new(
        pw,
        level,
        MepSection::new(MepDomain::CableTray, MepShape::Rect { w: 0.3, h: 0.08 }),
        vec![(11.2, 0.8, z + 3.3), (11.2, 7.2, z + 3.3)],
    ))
    .map_err(e)?;
    ed.reload().map_err(e)?;
    let _ = scene(ed);
    ed.selection = vec![Sel::Mep(main)];
    Ok(())
}

/// Plenum of the demo office: top of the suspended ceiling and underside of the structure, m
/// above the level.
pub const OFFICE_PLENUM: (f64, f64) = (2.5, 3.0);

/// A 12 x 8 m office east of `demo` (x 14..26): an air handling unit, four supply diffusers and
/// three return grilles with real flows, both networks auto-routed in the plenum by equal
/// friction. Returns the planned metrics of supply and return.
pub fn demo_office(ed: &mut Editor) -> Result<Vec<ddc_model::RouteMetrics>, String> {
    use ddc_model::{CompSpec, DeviceSpec, NetReq, RouteFrom, SystemKind};
    let e = |e: ddc_model::Error| e.to_string();
    let level = ed.level;
    let z = ed.elevation;
    let wt = ed
        .types
        .first()
        .map(|t| t.id)
        .ok_or_else(|| tr(ed.big.lang(), "mep3d.no_wall_type"))?;
    let m = &mut ed.model;
    for (a, b) in [
        ((14.0, 0.0), (26.0, 0.0)),
        ((26.0, 0.0), (26.0, 8.0)),
        ((26.0, 8.0), (14.0, 8.0)),
        ((14.0, 8.0), (14.0, 0.0)),
    ] {
        m.add_wall(&ddc_model::WallSpec {
            a,
            b,
            level,
            base_offset: 0.0,
            height: 3.6,
            wall_type: wt,
            offset0: None,
        })
        .map_err(e)?;
    }
    let put = |m: &mut Model, family: &str, ty: &str, at: (f64, f64, f64)| {
        let mut place = CompSpec::at(level, 0, at.0, at.1);
        place.at.2 = at.2;
        m.mep_place(&DeviceSpec {
            family: family.into(),
            ty: ty.into(),
            place,
            system: None,
            flow: None,
        })
        .map(|p| p.comp)
        .map_err(e)
    };
    let ahu = put(m, "Air Handling Unit", "2000x1000", (16.0, 4.0, z))?;
    // terminals hang under the plenum: their necks end at its bottom
    let neck = z + OFFICE_PLENUM.0 - 0.2;
    let mut supply = vec![];
    for (x, y) in [(19.0, 2.5), (19.0, 5.5), (23.0, 2.5), (23.0, 5.5)] {
        let c = put(m, "Supply Diffuser", "300x300", (x, y, neck))?;
        m.mep_set_port_flow(c, "Air", Some(250.0)).map_err(e)?;
        supply.push((c, "Air".to_string()));
    }
    let mut ret = vec![];
    for x in [18.0, 21.0, 24.0] {
        let c = put(m, "Exhaust Grille", "400x400", (x, 7.0, z + OFFICE_PLENUM.0 - 0.15))?;
        m.mep_set_port_flow(c, "Air", Some(1000.0 / 3.0)).map_err(e)?;
        ret.push((c, "Air".to_string()));
    }
    let net = |name: &str, kind, connector: &str, to| {
        let mut r = NetReq::new(
            name,
            kind,
            level,
            RouteFrom::Port {
                comp: ahu,
                connector: connector.into(),
            },
        );
        r.to = to;
        r.friction = Some(1.0);
        r.insulation = 0.025;
        r.plenum = Some((z + OFFICE_PLENUM.0, z + OFFICE_PLENUM.1));
        r
    };
    let reqs = [
        net("SA1", SystemKind::SupplyAir, "Supply", supply),
        net("RA1", SystemKind::ReturnAir, "Return", ret),
    ];
    let mut metrics = vec![];
    for r in &reqs {
        // planned in turn so the return keeps clear of the supply already built
        let p = m.mep_plan_net(r).map_err(e)?;
        m.mep_build(&p.build).map_err(e)?;
        metrics.push(p.metrics);
    }
    ed.reload().map_err(e)?;
    let _ = scene(ed);
    Ok(metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_office_has_supply_and_return_with_real_flows() {
        let mut ed = Editor::new_in_memory().unwrap();
        demo(&mut ed).unwrap();
        let ms = demo_office(&mut ed).unwrap();
        assert_eq!(ms.len(), 2);
        for mt in &ms {
            assert!(mt.failed.is_empty(), "{mt:?}");
            assert_eq!(mt.clashes, 0, "{mt:?}");
            assert!(mt.critical_pa.is_some_and(|p| p > 0.0), "{mt:?}");
            // horizontal mains in the plenum
            assert!(mt.z_max <= ed.elevation + OFFICE_PLENUM.1 + 1e-6, "{mt:?}");
        }
        let systems = ed.model.mep_systems().unwrap();
        for name in ["SA1", "RA1"] {
            let s = systems.iter().find(|s| s.name == name).expect(name);
            let c = ed.model.mep_check(s.id).unwrap();
            assert!((c.total - 1000.0).abs() < 1e-6, "{name}: {}", c.total);
            assert!(c.devices.iter().all(|d| d.connected), "{name}: {:?}", c.devices);
        }
    }

    #[test]
    fn mep_in_scene_selectable_and_partial_rebuild() {
        let mut ed = Editor::new_in_memory().unwrap();
        demo(&mut ed).unwrap();
        let sc = scene(&mut ed);
        assert!(
            ed.big.timing.arch_reused,
            "nothing changed since demo: {:?}",
            ed.big.timing
        );
        let Sel::Mep(main) = ed.selection[0] else {
            panic!()
        };
        let (a, b) = ed.element_range(Sel::Mep(main)).unwrap();
        assert_eq!(ed.sel_of_element(a), Some(Sel::Mep(main)));
        let parts: Vec<_> = sc
            .meshes
            .iter()
            .filter(|m| (a..b).contains(&m.element))
            .collect();
        assert!(
            parts.iter().any(|m| ddc_render::is_translucent(m.tag)),
            "insulation shell"
        );
        assert!(parts.iter().any(|m| !ddc_render::is_translucent(m.tag)));
        let mep = MEP_BASE..MEP_BASE + SPAN;
        assert!(
            sc.meshes
                .iter()
                .filter(|m| mep.contains(&m.element))
                .count()
                >= 6
        );
        // a size edit: networks only, architecture from the cache, fittings rebuilt
        let mut s = ed.model.mep_run(main).unwrap();
        s.section.shape = MepShape::Rect { w: 0.6, h: 0.35 };
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
            m.update_mep_run(main, &s)
        })))
        .unwrap();
        let sc2 = scene(&mut ed);
        let t = ed.big.timing.clone();
        assert!(
            t.arch_reused && t.systems_rebuilt == 1 && t.comps_reused,
            "{t:?}"
        );
        assert_eq!(ed.element_range(Sel::Mep(main)), Some((a, b)), "id stays");
        let wide = |sc: &Scene| {
            sc.meshes
                .iter()
                .filter(|m| m.element == a && !ddc_render::is_translucent(m.tag))
                .flat_map(|m| m.positions.iter())
                .map(|p| p[0])
                .fold(f32::MIN, f32::max)
        };
        assert!(wide(&sc2) > wide(&sc) + 0.02);
        // a grip move and a delete are undo steps
        ed.selection = vec![Sel::Mep(main)];
        let g = grips(&ed);
        let mv = g
            .iter()
            .find(|g| g.kind == crate::grips3d::GripKind::Move)
            .unwrap();
        apply_grip(
            &mut ed,
            crate::grips3d::Done {
                kind: mv.kind,
                from: mv.at,
                to: mv.at + Vec3::new(0.0, 0.5, 0.0),
                copy: false,
            },
        );
        assert!((ed.model.mep_run(main).unwrap().points[0].1 - 2.5).abs() < 1e-9);
        ed.delete_selection();
        assert!(ed.model.mep_run(main).is_err());
        ed.undo();
        assert!(ed.model.mep_run(main).is_ok());
        let _ = scene(&mut ed);
        assert!(ed.big.timing.arch_reused);
        assert!(!ed.big.outlines(Sel::Mep(main)).is_empty());
        assert!(crate::selection::all(&ed).contains(&Sel::Mep(main)));
    }

    #[test]
    fn section_fill_from_system_color() {
        assert_eq!(
            cut_fill("mep#000000"),
            Some(egui::Color32::from_rgb(63, 63, 63))
        );
        assert_eq!(cut_fill("concrete"), None);
    }

    const NB: char = '\u{a0}';

    #[test]
    fn slope_parse_accepts_percent_per_mille_and_ratio() {
        for (s, want) in [
            ("1.5", 1.5),
            ("1,5", 1.5),
            (" 1,5 % ", 1.5),
            ("2%", 2.0),
            ("5\u{2030}", 0.5),
            ("12,5 \u{2030}", 1.25),
            ("1:50", 2.0),
            ("1 : 100", 1.0),
            ("-2", -2.0),
            ("0", 0.0),
        ] {
            for dec in ['.', ','] {
                let got = slope_parse(s, dec).unwrap();
                assert!((got - want).abs() < 1e-9, "{s} -> {got}");
            }
        }
        assert_eq!(slope_parse("1.234,5", ','), Err(SLOPE_ERR), "group marks, then out of range");
        assert!((slope_parse("0,125", '.').unwrap() - 0.125).abs() < 1e-9);
    }

    #[test]
    fn slope_parse_errors_are_catalog_keys() {
        for s in ["", "  ", "abc", "1..2", "%", "1:0", "1:", ":5", "inf", "NaN", "1e9", "150", "1:0.5x"] {
            assert_eq!(slope_parse(s, '.'), Err(SLOPE_ERR), "{s:?}");
        }
        let en = ddc_ui::Catalog::new(Lang::En);
        let ru = ddc_ui::Catalog::new(Lang::Ru);
        for key in [SLOPE_ERR, REJECTED, "mep_dn.dn.unset"] {
            let (a, b) = (en.t(key), ru.t(key));
            assert!(a != key && b != key && a != b, "{key} missing or untranslated");
        }
        assert_eq!(en.t(SLOPE_ERR), "Enter a slope in %, e.g. 1.5");
        assert!(ru.t(SLOPE_ERR).contains('%'));
        for e in [en.t(SLOPE_ERR), ru.t(SLOPE_ERR)] {
            assert!(!e.contains("float") && !e.contains("invalid"), "no parser text: {e}");
        }
    }

    #[test]
    fn dn_text_follows_units_and_language() {
        use crate::units::Units;
        let en = ddc_ui::Catalog::new(Lang::En);
        let ru = ddc_ui::Catalog::new(Lang::Ru);
        assert_eq!(dn_text(Some(50), Units::Metric, &en), format!("50{NB}mm"));
        assert_eq!(dn_text(Some(50), Units::Metric, &ru), format!("50{NB}{}", ru.t("mep_units.unit.mm")));
        assert_eq!(dn_text(Some(50), Units::Imperial, &en), format!("1.97{NB}in"));
        assert_eq!(dn_text(Some(50), Units::Imperial, &ru), format!("1,97{NB}{}", ru.t("mep_units.unit.in")));
        assert_eq!(dn_text(None, Units::Metric, &en), "Not set");
        assert_eq!(dn_text(None, Units::Imperial, &ru), ru.t("mep_dn.dn.unset"));
        assert!(!dn_text(None, Units::Metric, &ru).is_empty(), "never an empty control");
    }

    #[test]
    fn dn_rows_select_the_current_size_or_the_placeholder() {
        use crate::units::Units;
        let en = ddc_ui::Catalog::new(Lang::En);
        let (rows, sel, off) = dn_items(&[15, 20, 25], Some(20), Units::Metric, &en);
        assert_eq!((rows.len(), sel, off), (3, 1, 0));
        assert_eq!(rows[sel], format!("20{NB}mm"));
        let (rows, sel, off) = dn_items(&[15, 20], None, Units::Metric, &en);
        assert_eq!((rows.len(), sel, off), (3, 0, 1));
        assert_eq!(rows[0], "Not set");
        assert_eq!(rows[1], format!("15{NB}mm"));
    }

    #[test]
    fn system_swatch_comes_from_the_system_colour() {
        use crate::tools::{mep_swatch_color, mep_system_items, MepMark};
        let sys = |id: i64, color: [u8; 3]| ddc_model::MepSystem {
            id,
            name: format!("S{id}"),
            kind: ddc_model::SystemKind::ALL[0],
            color,
        };
        let paper = egui::Color32::from_rgb(0xFB, 0xFB, 0xFA); // design-lint: data
        let rows = mep_system_items(&[sys(2, [200, 30, 30]), sys(1, [30, 30, 200])], "", MepMark::Dash);
        let color_of = |id: i64| {
            let r = rows.iter().find(|r| r.id == Some(id)).unwrap();
            mep_swatch_color(r.mark, paper)
        };
        assert_eq!(color_of(2), Some(egui::Color32::from_rgb(200, 30, 30)));
        assert_ne!(color_of(1), color_of(2));
        assert_eq!(rows[1].id, Some(1), "id order after the skipped first row");
    }

    #[test]
    fn run_system_redraw_writes_nothing_and_a_pick_is_one_undo_step() {
        let mut ed = Editor::new_in_memory().unwrap();
        demo(&mut ed).unwrap();
        let Sel::Mep(main) = ed.selection[0] else {
            panic!()
        };
        let before = ed.model.mep_run(main).unwrap();
        let ctx = egui::Context::default();
        // the panel paints named font weights: they exist from the pass after install
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let rev = ed.revision;
        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |c| {
                egui::CentralPanel::default().show(c, |ui| {
                    props(ui, &mut ed);
                });
            });
        }
        assert_eq!(ed.revision, rev, "a redraw of the run panel wrote to the model");
        assert_eq!(ed.model.mep_run(main).unwrap().system, before.system);
        // the panel's commit path: another system of the run's kind, one undo step back
        let kind = ed
            .model
            .mep_systems()
            .unwrap()
            .into_iter()
            .find(|s| s.id == before.system)
            .unwrap()
            .kind;
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.add_mep_system("xs34 other", kind).map(drop))))
            .unwrap();
        let other = ed
            .model
            .mep_systems()
            .unwrap()
            .into_iter()
            .find(|s| s.name == "xs34 other")
            .unwrap()
            .id;
        let s2 = RunSpec { system: other, ..before.clone() };
        ed.run_op(OpFn(Arc::new(move |m: &mut Model| m.update_mep_run(main, &s2))))
            .unwrap();
        assert_eq!(ed.model.mep_run(main).unwrap().system, other);
        ed.undo();
        assert_eq!(ed.model.mep_run(main).unwrap().system, before.system);
    }

    #[test]
    fn a_run_on_a_missing_system_shows_its_own_id_not_another_system() {
        let sys = |id: i64| ddc_model::MepSystem {
            id,
            name: format!("S{id}"),
            kind: ddc_model::SystemKind::ALL[0],
            color: [1, 2, 3],
        };
        let rows = run_system_rows(&[sys(1), sys(2)], 2, "gone");
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [Some(1), Some(2)]);
        let rows = run_system_rows(&[sys(1)], 7, "gone #7");
        assert_eq!((rows[0].id, rows[0].label.as_str()), (Some(7), "gone #7"));
        assert_eq!(rows[0].mark, crate::tools::MepMark::Missing, "warning tone in Properties");
        assert_eq!(rows.len(), 2);
        assert_eq!(crate::tools::mep_missing_label(false, 7), "Missing system (#7)");
        assert!(crate::tools::mep_missing_label(true, 7).ends_with("(#7)"));
    }

    #[test]
    fn run_panel_uses_kit_widgets_and_catalog_text_only() {
        let src = include_str!("mep3d.rs");
        let a = src.find("pub fn props(").unwrap();
        let b = a + src[a..].find("\nfn pick(").unwrap();
        let body = &src[a..b];
        for bad in [
            "egui::ComboBox",
            ".selectable_label(",
            "kv_widget(ui, \"",
            "kv_edit(",
            "e.to_string()",
            "{e:?}",
            "{:?}",
        ] {
            assert!(!body.contains(bad), "run panel still has {bad}");
        }
        assert!(body.contains("TextField::new(") && body.contains("choice("));
    }
}
