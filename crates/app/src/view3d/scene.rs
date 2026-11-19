//! The 3D view scene on top of the base one (built by `prepare_3d`): the same `Arc` meshes (GPU chunks and
//! the picking index recognise them), minus elements hidden by the open saved view, plus
//! section-box cut fills in the material colour. Category visibility and transparency are
//! not handled here: they use the mesh category index (`Scene::cats`) and the coverage table in the frame uniform.
//! Fills are cached by (box, base-scene build, palette); heavy computation goes to a
//! cancellable background thread, and while it runs (and while a box handle is dragged) the old fills are hidden.

use super::{sel_category, CATS};
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::ui_kit::Tokens;
use ddc_render::caps::{self, Budget};
use ddc_render::glam::Vec3;
use ddc_render::scene::mesh_box;
use ddc_render::style::cap_tag;
use ddc_render::{Aabb, Inst, Instanced, Scene, SceneMesh, VisualStyle};
#[cfg(feature = "gui")]
use ddc_render::Renderer;
use ddc_render::{TAG_DOOR, TAG_GLASS, TAG_MULLION, TAG_PANEL, TAG_SLAB, TAG_WINDOW};
use crate::RenderState;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use web_time::{Duration, Instant};

/// Fill computation budget on the UI thread, ms; anything longer goes to a background thread.
const LIVE_BUDGET_MS: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CapsKey {
    b: [f32; 6],
    gen: u64,
    poche: [[u8; 3]; 7],
}

/// Fills with a source index (a base-scene mesh or `meshes.len()` + group).
type Caps = Vec<(usize, Arc<SceneMesh>)>;
type Raw = Vec<(usize, SceneMesh)>;

struct Job {
    key: CapsKey,
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Option<Raw>>,
}

impl Drop for Job {
    fn drop(&mut self) {
        // new box or new scene: the previous computation is no longer needed
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(crate) struct Deco {
    /// Base scene (without fills and view-hidden elements).
    base: Option<Scene>,
    /// Category indices of the base scene: meshes, then instance groups.
    base_cats: Vec<u8>,
    /// Build number of the base scene.
    gen: u64,
    /// Build number of the scene handed to the viewport (a different one means the base was rebuilt).
    pub(crate) out_fp: Option<u64>,
    out_key: Option<(u64, u64, u64, bool)>,
    /// Bounds of the base scene.
    pub base_bounds: Option<Aabb>,
    /// Duration of the last fill computation on the UI thread, ms.
    pub cost_ms: f32,
    poche: Option<[[u8; 3]; 7]>,
    colors: HashMap<u32, [u8; 3]>,
    caps: Option<(CapsKey, Caps)>,
    caps_rev: u64,
    job: Option<Job>,
    /// Fills do not match the box (a handle is being dragged or a computation is running): the shader skips them.
    pub caps_stale: bool,
    /// Woken by a finished background fill, so the view does not poll while it runs.
    pub(crate) wake: Option<egui::Context>,
}

impl Deco {
    /// A background fill computation is running (the viewport keeps repainting to pick up the result).
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

pub(crate) fn fp(s: &Scene) -> u64 {
    s.serial()
}

fn rgb(c: egui::Color32) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// Category index for a vertex tag (1… by `CATS`; 0 — outside the rules).
pub fn cat_index(cat: &str) -> u8 {
    CATS.iter()
        .position(|c| *c == cat)
        .map_or(0, |i| i as u8 + 1)
}

/// Mesh category: from the pick element, otherwise from the tag (other levels, networks).
pub fn mesh_category(ed: &Editor, m: &SceneMesh) -> &'static str {
    element_category(ed, m.element, m.tag)
}

fn element_category(ed: &Editor, element: u32, tag: u32) -> &'static str {
    use crate::scene_ids::{range_of, EXT_BASE, EXT_END};
    // some 3D providers (roofs, columns, structure, edges) and terrain are not "walls" by tag
    if (EXT_BASE..=EXT_END).contains(&element) || range_of(element) == Some("topo") {
        return "other";
    }
    if let Some(s) = ed.sel_of_element(element) {
        return sel_category(ed, s);
    }
    tag_category(tag)
}

pub fn tag_category(tag: u32) -> &'static str {
    let t = ddc_render::style::base_tag(tag);
    if t & 0x8000_0000 != 0 {
        "other"
    } else if t & 0x4000_0000 != 0 {
        "mep"
    } else {
        match t {
            TAG_DOOR => "door",
            TAG_WINDOW => "window",
            TAG_GLASS | TAG_MULLION | TAG_PANEL => "curtain",
            TAG_SLAB => "floor",
            _ => "wall",
        }
    }
}

/// Scene category indices: meshes, then instance groups (by first placement).
pub fn categories(ed: &Editor, s: &Scene) -> Vec<u8> {
    let meshes = s.meshes.iter().map(|m| cat_index(mesh_category(ed, m)));
    let groups = s.instanced.iter().map(|g| {
        let e = g.inst.first().map_or(u32::MAX, |i| i.element);
        cat_index(element_category(ed, e, g.proto.tag))
    });
    meshes.chain(groups).collect()
}

/// Fill index (`Tokens::mat`, then 6 — the generic fill): the main layer is the first
/// load-bearing one (not insulation and not glass finish) in layer order.
fn mat_index(materials: &[String]) -> usize {
    let cls: Vec<usize> = materials
        .iter()
        .map(|m| crate::section_view::mat_class(m))
        .collect();
    cls.iter()
        .copied()
        .find(|c| *c != 2 && *c != 5)
        .or_else(|| cls.first().copied())
        .unwrap_or(6)
}

/// Section fill colour of an element (sRGB).
fn cap_color(ed: &Editor, element: u32, tag: u32, poche: &[[u8; 3]; 7]) -> [u8; 3] {
    let i = match ed.sel_of_element(element) {
        Some(Sel::Wall(id)) => ed
            .walls
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| ed.model.wall_type_materials(w.wall_type).ok())
            .map_or(6, |v| mat_index(&v)),
        Some(Sel::Slab(id)) => ed.model.slab_materials(id).map_or(6, |v| mat_index(&v)),
        Some(Sel::Opening(_)) => 4,
        Some(Sel::Curtain(_)) | Some(Sel::Panel(..)) => 3,
        _ => match ddc_render::style::base_tag(tag) {
            TAG_DOOR => 4,
            TAG_WINDOW | TAG_GLASS | TAG_MULLION | TAG_PANEL => 3,
            _ => 6,
        },
    };
    poche[i.min(6)]
}

/// Meshes without fill: lines, glass (thin, blended), networks with a shell.
fn cappable(m: &SceneMesh) -> bool {
    let t = m.tag;
    t & 0x8000_0000 == 0 && t != TAG_GLASS && t & 0x2000_0000 == 0
}

/// Whether a box face cuts the body's bounding box.
fn crosses(mb: &Aabb, b: &Aabb) -> bool {
    !mb.is_empty()
        && (0..3).any(|k| {
            (mb.min[k] < b.min[k] && b.min[k] < mb.max[k])
                || (mb.min[k] < b.max[k] && b.max[k] < mb.max[k])
        })
}

/// Scene for the viewport: base meshes as the same `Arc`s (no copies), except those hidden by the view;
/// fills carry the category of their source; instances are the same list if nothing is
/// hidden.
pub fn build(
    base: &Scene,
    cats: &[u8],
    hidden: &dyn Fn(u32) -> bool,
    caps: &[(usize, Arc<SceneMesh>)],
) -> Scene {
    let n = base.meshes.len();
    let cat = |i: usize| cats.get(i).copied().unwrap_or(0);
    let mut meshes = Vec::with_capacity(n + caps.len());
    let mut boxes = Vec::with_capacity(n + caps.len());
    let mut out_cats = Vec::with_capacity(n + caps.len() + base.instanced.len());
    for (i, (m, b)) in base.meshes.iter().zip(base.boxes()).enumerate() {
        if hidden(m.element) {
            continue;
        }
        meshes.push(Arc::clone(m));
        boxes.push(*b);
        out_cats.push(cat(i));
    }
    for (src, c) in caps {
        if hidden(c.element) {
            continue;
        }
        meshes.push(Arc::clone(c));
        boxes.push(mesh_box(c));
        out_cats.push(cat(*src));
    }
    let mut inst = Vec::with_capacity(base.instanced.len());
    for (g, grp) in base.instanced.iter().enumerate() {
        let keep: Vec<Inst> = grp
            .inst
            .iter()
            .filter(|i| !hidden(i.element))
            .copied()
            .collect();
        if keep.is_empty() {
            continue;
        }
        inst.push(if keep.len() == grp.inst.len() {
            grp.clone()
        } else {
            Instanced {
                inst: Arc::new(keep),
                ..grp.clone()
            }
        });
        out_cats.push(cat(n + g));
    }
    Scene::from_parts(meshes, boxes)
        .with_instanced(inst)
        .with_cats(out_cats)
}

#[cfg(feature = "gui")]
fn upload(rs: Option<&RenderState>, scene: &Scene) {
    if let Some(rs) = rs {
        if let Some(r) = rs.renderer.write().callback_resources.get_mut::<Renderer>() {
            r.set_scene(&rs.device, scene);
        }
    }
}

#[cfg(not(feature = "gui"))]
fn upload(rs: Option<&RenderState>, _: &Scene) {
    let _ = rs;
}

/// Whether the bounds are similar (a model edit rather than another project): the camera is then not reset.
fn same_place(a: &Aabb, b: &Aabb) -> bool {
    let (da, db) = ((a.max - a.min).length(), (b.max - b.min).length());
    ddc_render::nav::intersect(a, b).is_some() && db < da * 2.0 + 1.0 && da < db * 2.0 + 1.0
}

/// Fill colours (the model is available only on the UI thread) for bodies the box cuts.
fn fill_colors(
    base: &Scene,
    colors: &mut HashMap<u32, [u8; 3]>,
    ed: &Editor,
    b: &Aabb,
    poche: &[[u8; 3]; 7],
) {
    for (m, mb) in base.meshes.iter().zip(base.boxes()) {
        if cappable(m) && crosses(mb, b) {
            colors
                .entry(m.element)
                .or_insert_with(|| cap_color(ed, m.element, m.tag, poche));
        }
    }
    for g in base.instanced.iter().filter(|g| cappable(&g.proto)) {
        for i in g.inst.iter() {
            if crosses(&g.world_bounds(i), b) {
                colors
                    .entry(i.element)
                    .or_insert_with(|| cap_color(ed, i.element, g.proto.tag, poche));
            }
        }
    }
}

fn caps_of(
    base: &Scene,
    b: &Aabb,
    colors: &HashMap<u32, [u8; 3]>,
    fallback: [u8; 3],
    budget: &Budget,
) -> Result<Raw, caps::Over> {
    let tag = |m: &SceneMesh| {
        cappable(m).then(|| cap_tag(colors.get(&m.element).copied().unwrap_or(fallback)))
    };
    caps::scene_caps(base, b, tag, budget)
}

fn set_caps(caps: &mut Option<(CapsKey, Caps)>, rev: &mut u64, k: CapsKey, v: Raw) {
    *caps = Some((k, v.into_iter().map(|(i, m)| (i, Arc::new(m))).collect()));
    *rev += 1;
}

/// Fills for key `k`: from the cache, from a background computation, on the UI thread within budget, or via a new
/// background computation. Not recomputed while a handle is dragged on a heavy model.
fn update_caps(d: &mut Deco, ed: &Editor, k: CapsKey, pointer_down: bool) {
    let Deco {
        base,
        colors,
        caps,
        caps_rev,
        job,
        cost_ms,
        wake: d_wake,
        ..
    } = d;
    if caps.as_ref().is_some_and(|c| c.0 == k) {
        *job = None;
        return;
    }
    let polled = match job.as_ref() {
        Some(j) if j.key == k => Some(j.rx.try_recv()),
        _ => None,
    };
    if let Some(p) = polled {
        match p {
            Ok(r) => {
                set_caps(caps, caps_rev, k, r.unwrap_or_default());
                *job = None;
            }
            // the thread panicked — no fills for this box, no retries
            Err(mpsc::TryRecvError::Disconnected) => {
                set_caps(caps, caps_rev, k, vec![]);
                *job = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }
        return;
    }
    *job = None;
    if pointer_down && *cost_ms > LIVE_BUDGET_MS {
        return;
    }
    let Some(base) = base.as_ref() else {
        return;
    };
    let b = Aabb {
        min: Vec3::new(k.b[0], k.b[1], k.b[2]),
        max: Vec3::new(k.b[3], k.b[4], k.b[5]),
    };
    fill_colors(base, colors, ed, &b, &k.poche);
    let t0 = Instant::now();
    let live = Budget {
        deadline: Some(t0 + Duration::from_secs_f32(LIVE_BUDGET_MS / 1000.0)),
        cancel: None,
    };
    let fallback = k.poche[6];
    let r = caps_of(base, &b, colors, fallback, &live);
    *cost_ms = t0.elapsed().as_secs_f32() * 1000.0;
    if let Ok(v) = r {
        set_caps(caps, caps_rev, k, v);
        return;
    }
    // over budget: the same computation in the background (meshes are shared — only pointers are copied)
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let parts = (
        base.meshes.clone(),
        base.boxes().to_vec(),
        base.instanced.clone(),
    );
    let colors = colors.clone();
    let stop = cancel.clone();
    let wake = d_wake.clone();
    // pure computation on shared meshes; a crash drops the sender, read as "no fills"
    let spawned = crate::jobs::spawn_guarded(
        "ddc-caps",
        move || {
            let s = Scene::from_parts(parts.0, parts.1).with_instanced(parts.2);
            let budget = Budget {
                deadline: None,
                cancel: Some(&stop),
            };
            let _ = tx.send(caps_of(&s, &b, &colors, fallback, &budget).ok());
            if let Some(c) = wake {
                c.request_repaint();
            }
        },
        |_| {},
    );
    match spawned {
        Ok(_) => *job = Some(Job { key: k, cancel, rx }),
        Err(_) => set_caps(caps, caps_rev, k, vec![]),
    }
}

impl Deco {
    /// New base scene: categories, reset of the colour cache and computation. `true` — the bounds
    /// are similar to the previous ones (a model edit): the camera stays.
    pub(crate) fn set_base(&mut self, ed: &Editor, base: Scene) -> bool {
        let prev = self.base_bounds;
        self.base_bounds = base.bounds;
        self.base_cats = categories(ed, &base);
        self.gen += 1;
        // wall types and materials may have changed
        self.colors.clear();
        self.job = None;
        self.out_key = None;
        let keep = matches!((prev, base.bounds), (Some(p), Some(n)) if same_place(&p, &n));
        self.base = Some(base);
        keep
    }

    /// Fill step for the box (no box — no fills); sets `caps_stale`.
    pub(crate) fn step_caps(
        &mut self,
        ed: &Editor,
        b: Option<Aabb>,
        poche: [[u8; 3]; 7],
        pointer_down: bool,
    ) -> Option<CapsKey> {
        if self.poche != Some(poche) {
            // theme change: fill colours from the new palette
            self.poche = Some(poche);
            self.colors.clear();
        }
        let key = b.map(|b| CapsKey {
            b: [b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z],
            gen: self.gen,
            poche,
        });
        match key {
            Some(k) => {
                update_caps(self, ed, k, pointer_down);
                self.caps_stale = self.caps.as_ref().is_none_or(|c| c.0 != k);
            }
            None => {
                self.job = None;
                self.caps_stale = false;
            }
        }
        key
    }

    /// Scene for the viewport if its contents changed since the previous call.
    pub(crate) fn output(
        &mut self,
        ed: &Editor,
        key: Option<CapsKey>,
        vh: &HashSet<Sel>,
        hidden_rev: u64,
    ) -> Option<Scene> {
        let out_key = (self.gen, hidden_rev, self.caps_rev, key.is_some());
        if self.out_key == Some(out_key) {
            return None;
        }
        let base = self.base.as_ref()?;
        let hidden =
            |e: u32| !vh.is_empty() && ed.sel_of_element(e).is_some_and(|s| vh.contains(&s));
        let caps: &[(usize, Arc<SceneMesh>)] = match (&self.caps, key) {
            (Some((_, c)), Some(_)) => c,
            _ => &[],
        };
        let sc = build(base, &self.base_cats, &hidden, caps);
        self.out_fp = Some(fp(&sc));
        self.out_key = Some(out_key);
        Some(sc)
    }

    /// Base scene (before fills and 3D-view hiding): the shape preview is overlaid on it.
    pub(crate) fn base_scene(&self) -> Option<&Scene> {
        self.base.as_ref()
    }

    /// Cached fills (source index, mesh).
    pub(crate) fn cached_caps(&self) -> Option<&[(usize, Arc<SceneMesh>)]> {
        self.caps.as_ref().map(|c| c.1.as_slice())
    }
}

/// Theme fill palette: materials, then the generic one.
pub fn poche_of(t: &Tokens) -> [[u8; 3]; 7] {
    let mut poche = [[0u8; 3]; 7];
    for (i, c) in t.mat.iter().take(6).enumerate() {
        poche[i] = rgb(*c);
    }
    poche[6] = rgb(t.poche);
    poche
}

pub fn decorate(app: &mut EditorApp, rs: Option<&RenderState>, t: &Tokens) {
    super::index_job::poll(app);
    if Some(fp(&app.scene)) != app.view.nav.deco.out_fp {
        // the base scene was rebuilt (model edit, level or visibility change)
        let base = std::mem::take(&mut app.scene);
        if app.view.nav.deco.set_base(&app.ed, base) {
            app.view.keep_camera();
        }
    }
    let nav = &app.view.nav;
    let caps_box = app
        .view
        .section_box
        .filter(|_| nav.caps && nav.style != VisualStyle::Wireframe);
    let pointer_down = nav.pointer_down;
    let nav = &mut app.view.nav;
    let key = nav
        .deco
        .step_caps(&app.ed, caps_box, poche_of(t), pointer_down);
    let Some(sc) = nav
        .deco
        .output(&app.ed, key, &nav.view_hidden, nav.hidden_rev)
    else {
        return;
    };
    app.scene = sc;
    upload(rs, &app.scene);
    // the picking index is built from the final scene (same `Arc`s: the update is cheap)
    super::index_job::refresh(app);
}
