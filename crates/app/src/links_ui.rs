//! Linked models (read-only): the "Links" window, background load/reload of .rvt/.ifc,
//! placement and visibility (each edit is one undo operation), plan lines, 3D, snaps,
//! clashes with the host. Link elements are not selectable and are not written to the host model.
//! Hooks: `groups` (each link's own instance groups in 3D, separate from the host scene: a host
//! edit does not rebuild them), `paint_plan` (over the plan); snaps are a separate list
//! `ed.tool.link_walls` (`sync_snaps`). Loads run in a queue, no more than
//! `max_loads()` in parallel; the link cache is the `<project>.links` folder next to the project (`cache`).

use crate::ui_kit as kit;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use web_time::{Duration, SystemTime, UNIX_EPOCH};

use ddc_geom::V2;
use ddc_model::links::{link_format, Link, Transform};
use ddc_model::Model;

use crate::editor::{Editor, OpFn, Sel};
use crate::range_ui::Loc;
use crate::shell::EditorApp;
use crate::snap::SnapWall;
use crate::units::{active_units, DecimalMark, Units};
use crate::ui_kit::Tokens;

pub mod bench;
pub mod cache;
mod clash;
mod derive;
pub mod load;
pub mod source;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fed;
mod watch;

pub use derive::{LINK_ELEMENT, LINK_TAG, PLAN_CUT};

/// Simultaneous link loads: cores minus one (the UI thread), at least one.
pub fn max_loads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .saturating_sub(1)
        .max(1)
}

/// Source placement: the link position and the host level elevation (f64 bits).
type PlaceKey = (Transform, u64);

/// Links up to this many triangles are cut for the plan on the UI thread (well under 16 ms).
const INLINE_PLAN_TRIS: usize = 20_000;

type PlanCut = (Vec<(V2, V2)>, Vec<SnapWall>);

fn plan_of(elems: &[load::Elem], walls: &[load::LinkWall], xf: &Transform, zc: f64) -> PlanCut {
    let plan = derive::plan_lines(elems, xf, zc);
    let snaps = derive::snap_walls(walls, &plan, xf, zc);
    (plan, snaps)
}

struct Loaded {
    data: load::LinkData,
    /// The placement and cut elevation `plan` and `snaps` were built with.
    key: Option<(Transform, f64)>,
    plan: Vec<(V2, V2)>,
    snaps: Vec<SnapWall>,
    /// 3D groups at the last placement: the same `Arc`s while placement and level are unchanged.
    placed: Mutex<Option<(PlaceKey, Arc<Vec<ddc_render::Instanced>>)>>,
    /// Plan cut of a big link running off the UI thread, for this position and cut height.
    plan_job: Option<((Transform, f64), std::sync::mpsc::Receiver<PlanCut>)>,
}

impl Loaded {
    fn new(data: load::LinkData) -> Self {
        Self {
            data,
            key: None,
            plan: vec![],
            snaps: vec![],
            placed: Mutex::new(None),
            plan_job: None,
        }
    }

    /// 3D groups of link `id` at placement `xf` from elevation `z0`; recomputation is matrices only.
    fn groups(&self, id: i64, xf: &Transform, z0: f64) -> Arc<Vec<ddc_render::Instanced>> {
        let k = (*xf, z0.to_bits());
        let mut p = self.placed.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((pk, g)) = p.as_ref() {
            if *pk == k {
                return g.clone();
            }
        }
        let g = Arc::new(source::place(&self.data.source, id, xf, z0));
        *p = Some((k, g.clone()));
        g
    }
}

#[derive(Default)]
pub struct State {
    pub open: bool,
    /// A picked file that is neither .rvt nor .ifc, shown until the next link is added.
    refused: Option<String>,
    bridges: Option<crate::import::Bridges>,
    /// `model_link` rows at revision `rev`.
    list: Vec<Link>,
    rev: Option<u64>,
    loaded: HashMap<i64, Loaded>,
    jobs: HashMap<i64, load::LoadJob>,
    /// Autoload has already run (repeat via the "Reload" button).
    tried: HashSet<i64>,
    errors: HashMap<i64, String>,
    /// Placement fields: the placement they were filled with and the draft (applied by a button).
    edit: HashMap<i64, (Transform, Transform)>,
    icons: ddc_ui::icons::IconCache,
    clash_job: Option<clash::ClashJob>,
    clash: Option<(i64, Vec<clash::ClashRow>)>,
    snaps_dirty: bool,
    /// Links waiting to load (autoload, "reload all").
    queue: VecDeque<i64>,
    /// Watching link files for changes.
    watch: watch::Watch,
    /// Number of the loaded-link set for 3D: changes without a host edit.
    rev3d: u64,
    /// Loads finished in the current batch (for overall progress).
    batch_done: usize,
    /// Link cache folder next to the project file (None if the project is not saved).
    cache_dir: Option<PathBuf>,
    /// Links whose source hash is recomputed on the next load.
    rehash: HashSet<i64>,
}

impl State {
    /// Number of the link set in 3D (load, unload); host edits do not change it.
    pub fn rev3d(&self) -> u64 {
        self.rev3d
    }

    /// Overall link load progress: (done, total) in the current batch; None if nothing is loading.
    pub fn progress(&self) -> Option<(usize, usize)> {
        let left = self.jobs.len() + self.queue.len();
        (left > 0).then_some((self.batch_done, self.batch_done + left))
    }
}

enum Act {
    Add(String),
    Update(i64, Transform, bool),
    Remove(i64),
    Reload(i64),
    Clash(i64),
    Select(Sel, i64),
}

fn h_open(a: &mut EditorApp) {
    a.links.open = !a.links.open;
}

fn h_add(a: &mut EditorApp) {
    a.links.open = true;
}

fn h_reload_all(a: &mut EditorApp) {
    let ids: Vec<i64> = a.links.list.iter().map(|l| l.id).collect();
    for id in ids {
        a.links.rehash.insert(id);
        enqueue(&mut a.links, id);
    }
}

fn enqueue(st: &mut State, id: i64) {
    st.tried.insert(id);
    if !st.queue.contains(&id) {
        st.queue.push_back(id);
    }
}

/// Starts waiting loads while fewer than `max_loads()` are busy.
fn pump(st: &mut State) {
    let max = max_loads();
    while st.jobs.len() < max {
        let Some(id) = st.queue.pop_front() else {
            break;
        };
        if let Some(l) = st.list.iter().find(|l| l.id == id).cloned() {
            start_load(st, &l);
        }
    }
}

pub fn register(a: &mut EditorApp) {
    let list: &[(&str, crate::shell::Handler)] = &[
        ("mgr.links", h_open),
        ("mgr.link_add", h_add),
        ("mgr.link_reload", h_reload_all),
    ];
    for (id, h) in list {
        a.register_command(id, *h);
    }
}

pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = std::mem::take(&mut a.links);
    if st.open {
        let acts = window(&mut st, a, ctx);
        for act in acts {
            apply(&mut st, a, act);
        }
    }
    sync(&mut st, a, ctx);
    watch::tick(&mut st.watch, &st.list, a, ctx);
    a.links = st;
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn start_load(st: &mut State, l: &Link) {
    st.tried.insert(l.id);
    st.errors.remove(&l.id);
    let path = PathBuf::from(&l.path);
    let module = if l.format == "rvt" {
        let b = st.bridges.get_or_insert_with(crate::import::Bridges::load);
        let entries = crate::import::entries(&b.list);
        crate::import::entry_for(&entries, &path)
            .and_then(|e| e.dir.clone())
            .map(|d| (d, b.trust.clone()))
    } else {
        None
    };
    if let Some(j) = st.jobs.remove(&l.id) {
        j.cancel();
    }
    let cache = st.cache_dir.clone().map(|dir| load::CacheUse {
        dir,
        rehash: st.rehash.remove(&l.id),
    });
    st.jobs.insert(
        l.id,
        load::LoadJob::start(path, l.format.clone(), module, cache),
    );
}

fn report(e: &load::LoadError, a: &EditorApp) -> String {
    match e {
        load::LoadError::NoModule => a.sh.cat.t("links.err.no_module"),
        load::LoadError::Failed(m) => format!("{}: {m}", a.sh.cat.t("links.err.failed")),
    }
}

/// The link list from the model, loads, derived data (plan, snaps), clashes.
fn sync(st: &mut State, a: &mut EditorApp, ctx: &egui::Context) {
    let rev = a.ed.revision;
    st.cache_dir = a.ed.project.path().map(cache::dir_for);
    if st.rev != Some(rev) {
        st.rev = Some(rev);
        st.list = a.ed.model.links().unwrap_or_default();
        let ids: HashSet<i64> = st.list.iter().map(|l| l.id).collect();
        let had = st.loaded.len();
        st.loaded.retain(|id, _| ids.contains(id));
        if st.loaded.len() != had {
            st.rev3d += 1;
        }
        st.jobs.retain(|id, j| {
            let keep = ids.contains(id);
            if !keep {
                j.cancel();
            }
            keep
        });
        // after undoing a deletion the link returns with the same id and loads again
        st.tried.retain(|id| ids.contains(id));
        st.edit.retain(|id, _| ids.contains(id));
        if st.clash.as_ref().is_some_and(|c| !ids.contains(&c.0)) {
            st.clash = None;
        }
        st.queue.retain(|id| ids.contains(id));
        let todo: Vec<i64> = st
            .list
            .iter()
            .filter(|l| {
                !st.loaded.contains_key(&l.id)
                    && !st.jobs.contains_key(&l.id)
                    && !st.tried.contains(&l.id)
            })
            .map(|l| l.id)
            .collect();
        for id in todo {
            enqueue(st, id);
        }
        st.snaps_dirty = true;
    }
    let mut done = vec![];
    for (id, j) in st.jobs.iter_mut() {
        if let Some(r) = j.poll() {
            done.push((*id, r));
        }
    }
    if !st.jobs.is_empty() || !st.queue.is_empty() || st.clash_job.is_some() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    for (id, r) in done {
        st.jobs.remove(&id);
        let name = st
            .list
            .iter()
            .find(|l| l.id == id)
            .map_or_else(String::new, |l| l.name.clone());
        st.batch_done += 1;
        match r {
            Ok(data) => {
                let now = unix_now();
                let file = data.cache.clone().unwrap_or_default();
                let _ = a.ed.model.mark_link_loaded(id, now, &file);
                if let Some(l) = st.list.iter_mut().find(|l| l.id == id) {
                    l.loaded_at = now;
                    l.cache = file;
                }
                st.errors.remove(&id);
                st.loaded.insert(id, Loaded::new(data));
                if st.clash.as_ref().is_some_and(|c| c.0 == id) {
                    st.clash = None;
                }
                // only the link groups of the 3D scene change: the host scene is not rebuilt
                st.rev3d += 1;
                a.ed.status = Some(format!("{}: {name}", a.sh.cat.t("links.status.loaded")));
            }
            Err(e) => {
                let msg = report(&e, a);
                a.ed.status = Some(format!("{name}: {msg}"));
                st.errors.insert(id, msg);
            }
        }
        ctx.request_repaint();
    }
    pump(st);
    if st.jobs.is_empty() && st.queue.is_empty() {
        st.batch_done = 0;
    }
    let zc = a.ed.elevation + PLAN_CUT;
    let mut changed = false;
    for l in &st.list {
        let Some(ld) = st.loaded.get_mut(&l.id) else {
            continue;
        };
        let want = (l.xf, zc);
        if ld.key == Some(want) {
            continue;
        }
        if ld.data.source.tris <= INLINE_PLAN_TRIS {
            let (plan, snaps) = plan_of(&ld.data.elems, &ld.data.walls, &l.xf, zc);
            (ld.plan, ld.snaps, ld.key) = (plan, snaps, Some(want));
            changed = true;
            continue;
        }
        // a big link is cut off the UI thread; the previous lines stay until the new ones come
        if ld.plan_job.as_ref().is_none_or(|j| j.0 != want) {
            let (tx, rx) = std::sync::mpsc::channel();
            let (elems, walls, xf) = (ld.data.elems.clone(), ld.data.walls.clone(), l.xf);
            std::thread::spawn(move || {
                let _ = tx.send(plan_of(&elems, &walls, &xf, zc));
            });
            ld.plan_job = Some((want, rx));
        }
        if let Some((k, rx)) = &ld.plan_job {
            match rx.try_recv() {
                Ok((plan, snaps)) => {
                    (ld.plan, ld.snaps, ld.key) = (plan, snaps, Some(*k));
                    ld.plan_job = None;
                    changed = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                Err(_) => {
                    // the cut failed: keep what is shown, do not retry every frame
                    ld.key = Some(*k);
                    ld.plan_job = None;
                }
            }
        }
    }
    if changed {
        st.snaps_dirty = true;
        ctx.request_repaint();
    }
    sync_snaps(st, &mut a.ed);
    if let Some(j) = &mut st.clash_job {
        if let Some(res) = j.poll() {
            let link = j.link;
            st.clash_job = None;
            match res {
                Ok(rows) => {
                    let n = rows.len();
                    st.clash = Some((link, rows));
                    a.ed.status = Some(format!("{}: {n}", a.sh.cat.t("links.clash.count")));
                }
                Err(e) => {
                    st.clash = None;
                    a.ed.status = Some(format!("{}: {e}", a.sh.cat.t("links.clash.failed")));
                }
            }
        }
    }
}

/// Snaps of visible links go in a separate tool list (`tool.link_walls`), not in the
/// host wall snaps: rebuilding those does not overwrite them and does not depend on their count.
fn sync_snaps(st: &mut State, ed: &mut Editor) {
    if !st.snaps_dirty {
        return;
    }
    ed.tool.link_walls = st
        .list
        .iter()
        .filter(|l| l.visible)
        .filter_map(|l| st.loaded.get(&l.id))
        .flat_map(|ld| ld.snaps.iter().copied())
        .collect();
    st.snaps_dirty = false;
}

/// 3D hook: instance groups of visible loaded links (gray, not selectable), each link is
/// its own source: shapes are shared with the load, and a host edit returns the same `Arc`s.
pub fn groups(st: &State, ed: &Editor) -> Vec<ddc_render::Instanced> {
    if st.loaded.is_empty() {
        return vec![];
    }
    let links = ed.model.links().unwrap_or_default();
    let mut out = vec![];
    for l in links.iter().filter(|l| l.visible) {
        if let Some(ld) = st.loaded.get(&l.id) {
            out.extend(ld.groups(l.id, &l.xf, ed.elevation).iter().cloned());
        }
    }
    out
}

/// The host scene without links (shared parts): when the link set changes the host is not rebuilt.
pub struct HostBase {
    meshes: Vec<Arc<ddc_render::SceneMesh>>,
    boxes: Vec<ddc_render::Aabb>,
    inst: Vec<ddc_render::Instanced>,
    cats: Vec<u8>,
    focus: Option<ddc_render::Aabb>,
}

impl HostBase {
    pub fn of(sc: &ddc_render::Scene) -> Self {
        Self {
            meshes: sc.meshes.clone(),
            boxes: sc.boxes().to_vec(),
            inst: sc.instanced.clone(),
            cats: sc.cats.clone(),
            focus: sc.focus,
        }
    }

    pub fn scene(&self) -> ddc_render::Scene {
        let mut sc = ddc_render::Scene::from_parts(self.meshes.clone(), self.boxes.clone())
            .with_cats(self.cats.clone());
        sc.focus = self.focus;
        sc.with_instanced(self.inst.clone())
    }
}

/// The host scene plus link groups at the end of the instance list (host meshes and categories
/// are untouched; the "show all" frame includes links).
pub fn attach(st: &State, ed: &Editor, sc: ddc_render::Scene) -> ddc_render::Scene {
    let extra = groups(st, ed);
    if extra.is_empty() {
        return sc;
    }
    let mut sc = sc;
    let mut inst = std::mem::take(&mut sc.instanced);
    inst.extend(extra);
    sc.with_instanced(inst)
}

/// Plan hook: section lines of visible links, thin, halftone, over the plan in `rect`.
pub fn paint_plan(a: &EditorApp, ctx: &egui::Context, rect: egui::Rect, t: &Tokens) {
    let st = &a.links;
    let shown: Vec<&Loaded> = st
        .list
        .iter()
        .filter(|l| l.visible)
        .filter_map(|l| st.loaded.get(&l.id))
        .collect();
    if shown.is_empty() {
        return;
    }
    let painter = ctx
        .layer_painter(egui::LayerId::background())
        .with_clip_rect(rect);
    let c = rect.center();
    let (cx, cy, s) = (a.plan.center.x, a.plan.center.y, a.plan.scale);
    let to_s = |p: V2| egui::pos2(c.x + ((p.x - cx) * s) as f32, c.y - ((p.y - cy) * s) as f32);
    let stroke = egui::Stroke::new(1.0, t.text_dim.gamma_multiply(0.55));
    for ld in shown {
        for &(p, q) in &ld.plan {
            let (u, v) = (to_s(p), to_s(q));
            if rect.intersects(egui::Rect::from_two_pos(u, v)) {
                painter.line_segment([u, v], stroke);
            }
        }
    }
}

/// Largest offset a typed value may take, m.
const OFFSET_LIM: f64 = 100_000.0;
/// Rotation range of the field, degrees.
const ROT_LIM: f64 = 3600.0;

/// Edit text of the three offsets in project units and the rotation in degrees, in the
/// locale of `loc`. The window shows the same text through its fields; tests pin the round trip.
#[cfg(test)]
fn fields(xf: &Transform, units: Units, loc: Loc) -> [String; 4] {
    let len = |m: f64| kit::len_text(m, units, loc.mark);
    let deg = xf.rot.to_degrees();
    let rot = match loc.mark {
        DecimalMark::Comma => crate::units::fmt_num_ru(deg, 2),
        DecimalMark::Point => crate::units::fmt_num_en(deg, 2),
    };
    [len(xf.dx), len(xf.dy), len(xf.dz), rot]
}

/// Reads [`fields`] text back; any refused field gives `None`, so nothing is written.
#[cfg(test)]
fn parse_fields(buf: &[String; 4], units: Units, loc: Loc) -> Option<Transform> {
    let len = |s: &String| kit::len_commit(s, units, -OFFSET_LIM, OFFSET_LIM, loc.mark).ok();
    let deg = crate::units::parse_locale_number(buf[3].trim(), loc.mark).ok()?;
    Some(Transform {
        dx: len(&buf[0])?,
        dy: len(&buf[1])?,
        dz: len(&buf[2])?,
        rot: deg.to_radians(),
    })
}

/// True while the text typed into the length field `salt` is refused.
fn field_refused(ui: &egui::Ui, salt: egui::Id, units: Units, loc: Loc) -> bool {
    ui.data(|d| d.get_temp::<String>(salt.with("buf")))
        .is_some_and(|s| kit::len_commit(&s, units, -OFFSET_LIM, OFFSET_LIM, loc.mark).is_err())
}

/// Apply is live only for a changed, finite draft with no refused text in any field.
fn can_apply(refused: bool, draft: &Transform, base: &Transform) -> bool {
    !refused && draft != base && draft.is_finite()
}

/// Offset and rotation fields of link `id`, edited into `draft`. A value changes only on a
/// commit; refused text keeps the draft, shows the field error and is reported by the result.
fn offset_fields(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    units: Units,
    loc: Loc,
    id: i64,
    draft: &mut Transform,
) -> bool {
    let mut refused = false;
    let off = crate::tr_lang(loc.ru, "links.offset", &[]);
    let unit = match units {
        Units::Metric => crate::tr_str(loc.ru, "unit_fmt.unit.mm"),
        Units::Imperial => "",
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(&off);
        for (k, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
            let lid = ui.weak(axis).id;
            let salt = egui::Id::new(("ux57e.off", id, k));
            let mut v = [draft.dx, draft.dy, draft.dz][k];
            let name = format!("{off} {axis}");
            let done = kit::fixed_cell(ui, ddc_ui::theme::layout::PROP_VALUE, |ui| {
                kit::len_field(ui, th, salt, &mut v, units, -OFFSET_LIM, OFFSET_LIM, loc.ru, lid, &name)
            });
            if done {
                match k {
                    0 => draft.dx = v,
                    1 => draft.dy = v,
                    _ => draft.dz = v,
                }
            }
            refused |= field_refused(ui, salt, units, loc);
        }
        if !unit.is_empty() {
            ui.weak(unit);
        }
    });
    ui.horizontal(|ui| {
        let label = crate::tr_lang(loc.ru, "links.rot", &[]);
        let lid = ui.label(&label).id;
        let mut deg = draft.rot.to_degrees();
        if crate::range_ui::number_field(ui, ("ux57e.rot", id), &mut deg, 2, 1.0, (-ROT_LIM, ROT_LIM), "\u{b0}", loc, (lid, &label)) {
            draft.rot = deg.to_radians();
        }
        refused |= ddc_ui::widgets::NumberField::refused(ui, ("ux57e.rot", id), loc.format());
    });
    refused
}

fn status_line(st: &State, l: &Link, a: &EditorApp) -> String {
    let t = |k: &str| a.sh.cat.t(k);
    if let Some(j) = st.jobs.get(&l.id) {
        let (d, n) = j.progress;
        return match n {
            0 => t("links.loading"),
            n => format!("{} {}%", t("links.loading"), d * 100 / n.max(1)),
        };
    }
    if let Some(e) = st.errors.get(&l.id) {
        return e.clone();
    }
    match (st.loaded.contains_key(&l.id), l.loaded_at) {
        (true, at) if at > 0 => {
            let when = UNIX_EPOCH + Duration::from_secs(at as u64);
            format!(
                "{} {}",
                t("links.loaded"),
                crate::shell::recover_win::fmt_time(when)
            )
        }
        (true, _) => t("links.loaded"),
        _ => t("links.not_loaded"),
    }
}

/// An empty window is a form that fits its content; a list of links is a resizable tool window.
fn window_size(list_empty: bool) -> kit::DialogSize {
    if list_empty {
        kit::DialogSize::Form
    } else {
        kit::DialogSize::Tool
    }
}

/// Native file dialog for one link file; `None` when cancelled.
fn pick_link_file(ru: bool) -> Option<String> {
    crate::file_pick::Dlg::new()
        .add_filter("RVT / IFC", &["rvt", "ifc"])
        .set_title(crate::tr_lang(ru, "links.pick_title", &[]))
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// A picked file is linked at once when its format is known; otherwise its name is kept in
/// `refused` so the window can say why nothing happened.
fn picked(refused: &mut Option<String>, file: Option<String>) -> Option<Act> {
    let f = file?;
    if link_format(&f).is_some() {
        return Some(Act::Add(f));
    }
    *refused = Some(f);
    None
}

/// File name of a refused pick, for the notice.
fn refused_note(path: &str, ru: bool) -> String {
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    format!("{name}: {}", crate::tr_lang(ru, "links.format_bad", &[]))
}

/// Top of the window: the read-only note, the refusal of the last pick and, when nothing is
/// linked, the empty state whose one action opens the picker. True when that action was hit.
fn intro(
    ui: &mut egui::Ui,
    th: &ddc_ui::Theme,
    refused: &Option<String>,
    icons: &mut ddc_ui::icons::IconCache,
    ru: bool,
    list_empty: bool,
) -> bool {
    let tl = |k: &str| crate::tr_lang(ru, k, &[]);
    // secondary text token, not `weak`: the faded default fails contrast on the dialog body
    let note = th.rich(ddc_ui::theme::Role::Body, &tl("links.readonly"));
    ui.add(egui::Label::new(note.color(th.sem().text_secondary)).wrap());
    if let Some(f) = refused {
        kit::error_note(ui, &refused_note(f, ru));
    }
    if !list_empty {
        return false;
    }
    let (title, hint, label) = (tl("links.empty"), tl("links.empty_hint"), tl("links.add_pick"));
    ddc_ui::widgets::empty_state(ui, th, Some((icons, "link")), &title, &hint, Some(&label))
}

/// Share of a link's title row that its name may take.
const NAME_SHARE: f32 = 0.45;

fn window(st: &mut State, a: &EditorApp, ctx: &egui::Context) -> Vec<Act> {
    let t = |k: &str| a.sh.cat.t(k);
    let mut acts = vec![];
    let mut open = st.open;
    let empty = st.list.is_empty();
    kit::Dialog::new("links", t("links.title"), window_size(empty))
        .stable()
        .open(&mut open)
        .show(ctx, |ui| {
            let ru = matches!(a.sh.cat.lang, ddc_ui::Lang::Ru);
            let th = ddc_ui::theme::of(ctx);
            let (mut add_empty, mut add_foot) = (false, false);
            let mut body = |ui: &mut egui::Ui| {
                let State { refused, icons, list, .. } = &mut *st;
                add_empty = intro(ui, &th, refused, icons, ru, list.is_empty());
                if let Some((d, n)) = st.progress() {
                    let label = format!("{} {d} / {n}", t("links.loading_all"));
                    ddc_ui::widgets::progress(ui, &th, Some(d as f32 / n.max(1) as f32), &label);
                }
                for l in &st.list {
                    let status = status_line(&*st, l, a);
                    let mut draft = match st.edit.get(&l.id) {
                        Some((x, d)) if *x == l.xf => *d,
                        _ => l.xf,
                    };
                    ui.push_id(l.id, |ui| {
                        ui.group(|ui| {
                            ui.horizontal(|ui| {
                                let mut vis = l.visible;
                                let label = t("links.visible");
                                let r = ddc_ui::widgets::toggle(ui, &th, &mut vis, &label);
                                if r.changed() {
                                    acts.push(Act::Update(l.id, l.xf, vis));
                                }
                                let w = ui.available_width() * NAME_SHARE;
                                kit::fixed_cell(ui, w, |ui| {
                                    kit::name_with_tip(ui, &l.name, &l.path, true);
                                });
                                ui.label(format!(".{}", l.format));
                                kit::elided_weak(ui, &status);
                            });
                            kit::elided_weak(ui, &l.path);
                            if watch::row(ui, &mut st.watch, l.id, &t) {
                                acts.push(Act::Reload(l.id));
                            }
                            if let Some(ld) = st.loaded.get(&l.id) {
                                let levels = ld.data.levels;
                                ui.weak(format!(
                                    "{} {levels} · {} {}",
                                    t("links.levels"),
                                    t("links.elements"),
                                    ld.data.elems.len()
                                ));
                            }
                            let units = active_units();
                            let refused =
                                offset_fields(ui, &th, units, Loc::of(ru), l.id, &mut draft);
                            ui.horizontal(|ui| {
                                let can = can_apply(refused, &draft, &l.xf);
                                let label = t("links.apply");
                                let clicked = ui
                                    .add_enabled_ui(can, |ui| {
                                        if can {
                                            ddc_ui::widgets::Button::primary(&label).show(ui, &th)
                                        } else {
                                            ddc_ui::widgets::Button::secondary(&label).show(ui, &th)
                                        }
                                    })
                                    .inner
                                    .clicked();
                                if clicked && can {
                                    acts.push(Act::Update(l.id, draft, l.visible));
                                }
                                if kit::secondary_button(ui, t("links.reload")).clicked() {
                                    acts.push(Act::Reload(l.id));
                                }
                                let can = st.loaded.contains_key(&l.id) && st.clash_job.is_none();
                                let clash = t("links.clash");
                                if ui
                                    .add_enabled_ui(can, |ui| kit::secondary_button(ui, clash))
                                    .inner
                                    .on_disabled_hover_text(t("dlg.links.clash_off"))
                                    .clicked()
                                {
                                    acts.push(Act::Clash(l.id));
                                }
                                if kit::danger_button(ui, t("links.remove"), true).clicked() {
                                    acts.push(Act::Remove(l.id));
                                }
                            });
                        });
                    });
                    st.edit.insert(l.id, (l.xf, draft));
                }
                if st.clash_job.is_some() {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("links.clash.running"));
                    });
                }
                if let Some((lid, rows)) = &st.clash {
                    ui.separator();
                    let name = st
                        .list
                        .iter()
                        .find(|l| l.id == *lid)
                        .map_or("", |l| l.name.as_str());
                    ui.strong(format!(
                        "{} · {name}: {}",
                        t("links.clash.count"),
                        rows.len()
                    ));
                    if rows.is_empty() {
                        ui.weak(t("links.clash.none"));
                    }
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for r in rows {
                                let (hk, hid) = load::host_key(r.sel);
                                let d = [0, 1, 2].map(|k| r.max[k] - r.min[k]);
                                let text = format!(
                                    "{} {hid}  ↔  {} {} {}  ({:.2}×{:.2}×{:.2})",
                                    t(&format!("links.kind.{hk}")),
                                    t(&format!("links.kind.{}", r.kind)),
                                    r.id,
                                    r.guid,
                                    d[0],
                                    d[1],
                                    d[2]
                                );
                                if ui
                                    .selectable_label(false, text)
                                    .on_hover_text(t("links.clash.pick"))
                                    .clicked()
                                {
                                    acts.push(Act::Select(r.sel, r.level));
                                }
                            }
                        });
                }
            };
            if empty {
                body(ui);
            } else {
                kit::pinned_footer(ui, |ui, footer| {
                    if footer {
                        add_foot = kit::primary_action(ui, t("links.add_pick"), None);
                    } else {
                        body(ui);
                    }
                });
            }
            if add_empty || add_foot {
                acts.extend(picked(&mut st.refused, pick_link_file(ru)));
            }
        });
    st.open = open;
    acts
}

/// The window alone (no loads, no model sync) for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = std::mem::take(&mut a.links);
    let _ = window(&mut st, a, ctx);
    a.links = st;
}

/// Puts one not-loaded link per path into the open window's list.
#[cfg(test)]
pub(crate) fn test_list(a: &mut EditorApp, paths: &[String]) {
    a.links.list = paths
        .iter()
        .zip(1i64..)
        .map(|(p, id)| Link {
            id,
            path: p.clone(),
            name: p.rsplit(['/', '\\']).next().unwrap_or(p).to_string(),
            format: link_format(p).unwrap_or("ifc").to_string(),
            xf: Transform::default(),
            visible: true,
            loaded_at: 0,
            cache: String::new(),
        })
        .collect();
}

fn op_status(a: &mut EditorApp, r: Result<(), ddc_model::Error>, ok: &str) {
    a.ed.status = Some(match r {
        Ok(()) => a.sh.cat.t(ok),
        Err(e) => format!("{}: {e}", a.sh.cat.t("links.err.op")),
    });
}

fn apply(st: &mut State, a: &mut EditorApp, act: Act) {
    match act {
        Act::Add(path) => {
            st.refused = None;
            let f = OpFn(Arc::new(move |m: &mut Model| {
                m.add_link(&path, "", Transform::default()).map(|_| ())
            }));
            let r = a.ed.run_op(f);
            op_status(a, r, "links.status.added");
        }
        Act::Update(id, xf, vis) => {
            let f = OpFn(Arc::new(move |m: &mut Model| m.update_link(id, xf, vis)));
            let r = a.ed.run_op(f);
            op_status(a, r, "links.status.moved");
        }
        Act::Remove(id) => {
            let f = OpFn(Arc::new(move |m: &mut Model| m.remove_link(id)));
            let r = a.ed.run_op(f);
            op_status(a, r, "links.status.removed");
        }
        Act::Reload(id) => {
            if let Some(l) = st.list.iter().find(|l| l.id == id).cloned() {
                // an explicit reload reads the source anew: a same-size, same-time edit is seen
                st.rehash.insert(id);
                start_load(st, &l);
            }
        }
        Act::Clash(id) => {
            let (Some(l), Some(ld)) = (st.list.iter().find(|l| l.id == id), st.loaded.get(&id))
            else {
                return;
            };
            st.clash = None;
            match clash::ClashJob::start(&a.ed, id, ld.data.elems.clone(), l.xf) {
                Ok(j) => st.clash_job = Some(j),
                Err(e) => a.ed.status = Some(format!("{}: {e}", a.sh.cat.t("links.err.failed"))),
            }
        }
        Act::Select(sel, level) => {
            if a.ed.level != level {
                a.ed.set_level(level);
            }
            a.ed.selection = vec![sel];
            // the row must also bring the element into view, not only switch the level
            crate::open::summary::frame_selection(a);
        }
    }
}
