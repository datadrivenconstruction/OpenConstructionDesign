//! Clipboard, match properties, join, pin and groups: commands and the paste ghost.
//! The snapshot lives in the process and in the system clipboard, so it survives a project switch
//! and reaches another app instance. Each action is one `run_op`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ddc_geom::{v2, V2};
use ddc_model::{
    Clip, ClipText, Error, GroupInfo, Model, PasteOpts, PasteReport, PlaceLevel, CLIP_TEXT_MAX,
};

use crate::edit3d::pick::{Cat, Target as Pick};
use crate::editor::bodies::BodyKind;
use crate::editor::{Editor, OpFn, Sel};
use crate::shell::EditorApp;
use crate::wall_tool::{Event, Key, Mode};

/// Above this size the snapshot is not put on the system clipboard, only a marker: paste takes it from the process.
const OS_TEXT_MAX: usize = 8 * 1024 * 1024;
/// A ghost with many segments is drawn thinned out: it follows the cursor every frame.
const GHOST_PAINT: usize = 4_000;

/// Process snapshot: shared by all projects of this app instance.
static CLIP: Mutex<Option<Arc<Clip>>> = Mutex::new(None);
/// Text for the system clipboard from the background thread: (copy number, text).
static OUTBOX: Mutex<Option<(u64, String)>> = Mutex::new(None);
/// Number of the latest copy: late text from an older copy is discarded.
static GEN: AtomicU64 = AtomicU64::new(0);

type Items = Vec<(String, i64)>;

enum Dialog {
    Levels(Vec<(i64, String, bool)>),
    GroupName(String),
    GroupPlace(Vec<GroupInfo>),
    GroupDelete(Vec<GroupInfo>),
}

/// Where the last click landed: Ctrl+C/X/V arrive as events, not keys, with no area attached.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Focus {
    #[default]
    Canvas,
    Table,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ClipEv {
    Copy,
    Cut,
    Paste,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Route {
    Model,
    TableCopy,
    TablePaste,
    Ignore,
}

/// The table has focus (Ctrl+D and other table keys).
pub fn table_focused() -> bool {
    with(|s| s.focus == Focus::Table)
}

/// Over a window or in a table, cut and paste never touch model elements.
fn route(focus: Focus, over_window: bool, ev: ClipEv) -> Route {
    if over_window {
        return Route::Ignore;
    }
    match (focus, ev) {
        (Focus::Canvas, _) => Route::Model,
        (Focus::Table, ClipEv::Copy) => Route::TableCopy,
        (Focus::Table, ClipEv::Paste) => Route::TablePaste,
        (Focus::Table, _) => Route::Ignore,
    }
}

/// Text from the system clipboard at paste time.
#[derive(Debug, PartialEq)]
enum Incoming {
    Use(Box<Clip>),
    /// Marker of this process: the snapshot is already here.
    Own,
    TooBig,
    Foreign,
    Damaged,
}

fn incoming(text: &str, own: &str) -> Incoming {
    match Clip::parse(text) {
        Ok(ClipText::Foreign) => Incoming::Foreign,
        Ok(ClipText::Marker(tag)) if tag.starts_with(own) && !tag.is_empty() => Incoming::Own,
        Ok(ClipText::Marker(_)) => Incoming::TooBig,
        Ok(ClipText::Clip(c)) => Incoming::Use(c),
        Err(_) if text.len() > CLIP_TEXT_MAX => Incoming::TooBig,
        Err(_) => Incoming::Damaged,
    }
}

fn own_tag() -> String {
    format!("{}-", std::process::id())
}

/// What is waiting for a click in the plan.
enum Armed {
    Paste(Arc<Clip>),
    Match((String, i64)),
    Place(i64, Arc<Clip>),
}

#[derive(Default)]
struct State {
    armed: Option<Armed>,
    cursor: Option<V2>,
    dialog: Option<Dialog>,
    /// The system clipboard text is still being prepared: paste uses the process snapshot, not the stale text.
    pending: bool,
    /// An instance is being edited: members added and removed before it is finished.
    editing: Option<(i64, Items, Items)>,
    focus: Focus,
    /// Table rectangle from the last frame.
    table_rect: Option<egui::Rect>,
    project: Option<String>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn with<T>(f: impl FnOnce(&mut State) -> T) -> T {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn t(key: &str) -> String {
    crate::tr(key, &[])
}

fn clip_now() -> Option<Arc<Clip>> {
    CLIP.lock().ok().and_then(|c| c.clone())
}

fn set_clip(c: Clip) -> Arc<Clip> {
    let c = Arc::new(c);
    if let Ok(mut g) = CLIP.lock() {
        *g = Some(c.clone());
    }
    c
}

/// Serializing a 20k-element snapshot must not hold up the frame.
fn publish(c: Arc<Clip>) {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    with(|s| s.pending = true);
    // serializes a snapshot copy; the clipboard of the app itself is untouched
    let _ = crate::jobs::spawn_guarded(
        "clip-publish",
        move || {
            let text = c.to_text();
            let text = if text.len() > OS_TEXT_MAX {
                Clip::marker(&format!("{}{gen}", own_tag()))
            } else {
                text
            };
            if GEN.load(Ordering::SeqCst) == gen {
                if let Ok(mut g) = OUTBOX.lock() {
                    *g = Some((gen, text));
                }
            }
        },
        // the same marker as for an oversized snapshot: pending is cleared, nothing is lost
        move |_| {
            if GEN.load(Ordering::SeqCst) == gen {
                if let Ok(mut g) = OUTBOX.lock() {
                    *g = Some((gen, Clip::marker(&format!("{}{gen}", own_tag()))));
                }
            }
        },
    );
}

/// Data-table rectangle for the next clipboard event dispatch.
pub(crate) fn note_table(rect: egui::Rect) {
    with(|s| s.table_rect = Some(rect));
}

/// Clears everything waiting for a click: another command cancels paste and match, but not a group edit.
pub fn reset() {
    with(|s| s.armed = None);
}

/// Command of what waits for a click, while `handle` would take it (`interact::Scope`).
pub fn armed_command(ed: &Editor) -> Option<&'static str> {
    if !alive(ed) {
        return None;
    }
    with(|s| match s.armed {
        Some(Armed::Paste(_)) => Some("edit.paste"),
        Some(Armed::Match(_)) => Some("mod.match"),
        Some(Armed::Place(..)) => Some("mod.group_place"),
        None => None,
    })
}

/// Selection codes for rebar and analytical tracks: the table does not match the structural kind.
const EXT_TABLES: [(u16, &str); 3] = [
    (0x1B10, "rebar_set"),
    (0x1B11, "analytical_adj"),
    (crate::scene_ids::EXT_TOPO, "toposolid"),
];

pub(crate) fn sel_item(s: Sel) -> Option<(String, i64)> {
    let (k, id): (&str, i64) = match s {
        Sel::Wall(i) => ("wall", i),
        Sel::Opening(i) => ("opening", i),
        Sel::Curtain(i) | Sel::Panel(i, _) => ("curtain_wall", i),
        Sel::Slab(i) => ("slab", i),
        Sel::Room(i) => ("room", i),
        Sel::Mass(i) => ("mass", i),
        Sel::Mep(i) => ("mep_run", i),
        Sel::Comp(i) => ("comp", i),
        Sel::Anno(i) => ("anno", i),
        Sel::Body(k, i) => (k.table(), i),
        Sel::Ext(c, i) => {
            if c == crate::scene_ids::EXT_SOLID {
                ("solid", i)
            } else if let Some(x) = EXT_TABLES.iter().find(|x| x.0 == c) {
                (x.1, i)
            } else {
                (struct_table(crate::editor::structx::kind_of(c)?), i)
            }
        }
    };
    Some((k.to_string(), id))
}

fn struct_table(kind: &'static str) -> &'static str {
    if kind == "footing" {
        "iso_footing"
    } else {
        kind
    }
}

pub(crate) fn pick_item(p: Pick) -> Option<(String, i64)> {
    if let Some(s) = p.sel() {
        return sel_item(s);
    }
    let k = match p.cat {
        Cat::Roof => "roof",
        Cat::Stair => "stair",
        Cat::Railing => "railing",
        Cat::Ceiling => "ceiling",
        Cat::Column => "col",
        Cat::Beam => "beam",
        Cat::Mesh => "mesh",
        _ => return None,
    };
    Some((k.to_string(), p.id))
}

fn edge_items(a: &EditorApp) -> Items {
    crate::roof_edge_scene::selected(a)
        .into_iter()
        .map(|e| {
            let k = if e.kind() == "taper" {
                "roof_taper"
            } else {
                e.kind()
            };
            (k.to_string(), e.id())
        })
        .collect()
}

/// What Delete takes from the 3D selection. Unlike `items`, a panel stays a sub-element.
pub(crate) fn delete_extra(a: &EditorApp) -> crate::editor::delete::Extra {
    let mut x = crate::editor::delete::Extra::default();
    for p in &a.view.e3.sel {
        if let Some(s @ Sel::Panel(..)) = p.sel() {
            x.panels.push(s);
            continue;
        }
        let item = match p.cat {
            Cat::Ext(c) => sel_item(Sel::Ext(c, p.id)),
            _ => pick_item(*p),
        };
        match item {
            Some(i) => x.items.push(i),
            None if p.cat == Cat::Ext(crate::scene_ids::EXT_ROOF_EDGE) => {}
            None => x.refused += 1,
        }
    }
    x.items.extend(edge_items(a));
    x
}

/// Everything selected in the plan and in 3D: (table, id) without repeats.
fn items(a: &EditorApp) -> Items {
    let mut out: Items = a.ed.selection.iter().filter_map(|s| sel_item(*s)).collect();
    out.extend(a.view.e3.sel.iter().filter_map(|p| pick_item(*p)));
    if let Some(s) = crate::solid_tools::selected() {
        out.push(("solid".to_string(), s));
    }
    out.extend(edge_items(a));
    let mut seen = std::collections::HashSet::new();
    out.retain(|x| seen.insert(x.clone()));
    out
}

fn refs(v: &[(String, i64)]) -> Vec<(&str, i64)> {
    v.iter().map(|x| (x.0.as_str(), x.1)).collect()
}

pub(crate) fn sel_of(table: &str, id: i64) -> Option<Sel> {
    Some(match table {
        "wall" => Sel::Wall(id),
        "opening" => Sel::Opening(id),
        "curtain_wall" => Sel::Curtain(id),
        "slab" => Sel::Slab(id),
        "room" => Sel::Room(id),
        "mass" => Sel::Mass(id),
        "mep_run" => Sel::Mep(id),
        "comp" => Sel::Comp(id),
        "anno" => Sel::Anno(id),
        "iso_footing" => return crate::editor::structx::sel("footing", id),
        "strip_footing" | "raft" | "brace" | "truss" | "beam_system" | "ramp" | "zone" | "roof"
        | "shaft" | "stair" | "railing" | "grid" | "ref_plane" => {
            return crate::editor::structx::sel(table, id)
        }
        "solid" => Sel::Ext(crate::scene_ids::EXT_SOLID, id),
        t => Sel::Body(BodyKind::from_table(t)?, id),
    })
}

fn report(ed: &mut Editor, e: &Error) {
    crate::constraints_ui::show(ed, e);
}

/// The selection is editable only in select mode: another tool clears the click wait.
fn alive(ed: &Editor) -> bool {
    !ed.tool_armed()
}

/// Runs the action as one undo step and writes the result to the status line.
fn run(
    ed: &mut Editor,
    ok: &str,
    f: impl Fn(&mut Model) -> Result<(), Error> + Send + Sync + 'static,
) -> bool {
    match ed.run_op(OpFn(Arc::new(f))) {
        Ok(()) => {
            ed.status = Some(t(ok));
            true
        }
        Err(e) => {
            report(ed, &e);
            false
        }
    }
}

/// Selection with whole group instances; while an instance is being edited, its members are taken one by one.
fn expanded(a: &EditorApp) -> Items {
    let list = items(a);
    if with(|s| s.editing.is_some()) {
        return list;
    }
    let got = a.ed.model.group_expand(&refs(&list));
    got.unwrap_or(list)
}

fn copy(a: &mut EditorApp) -> bool {
    let list = expanded(a);
    if list.is_empty() {
        a.ed.status = Some(t("clip.nothing_selected"));
        return false;
    }
    match a.ed.model.clip_copy(&refs(&list)) {
        Ok(c) => {
            let skipped = c.unsupported.len();
            let n = list.len() - skipped;
            publish(set_clip(c));
            a.ed.status = Some(if skipped == 0 {
                crate::tr("clip.copied", &[("n", &n.to_string())])
            } else {
                crate::tr(
                    "clip.copied_partial",
                    &[("n", &n.to_string()), ("skipped", &skipped.to_string())],
                )
            });
            true
        }
        Err(e) => {
            report(&mut a.ed, &e);
            false
        }
    }
}

fn cut(a: &mut EditorApp) {
    // a panel is selected as its curtain wall: cutting would remove the whole wall
    let panel = |s: Option<Sel>| matches!(s, Some(Sel::Panel(..)));
    if a.ed.selection.iter().any(|s| panel(Some(*s)))
        || a.view.e3.sel.iter().any(|p| panel(p.sel()))
    {
        a.ed.status = Some(t("clip.cut_panel"));
        return;
    }
    let list = expanded(a);
    if list.is_empty() {
        a.ed.status = Some(t("clip.nothing_selected"));
        return;
    }
    let clip = match a.ed.model.clip_copy(&refs(&list)) {
        Ok(c) => c,
        Err(e) => {
            report(&mut a.ed, &e);
            return;
        }
    };
    // anything not captured in the snapshot is not deleted: there would be no way to restore it
    let gone: Items = list
        .into_iter()
        .filter(|x| !clip.unsupported.contains(x))
        .collect();
    let ok = run(&mut a.ed, "clip.cut_done", move |m| {
        m.delete_items("wall_cut", &refs(&gone))
    });
    // the buffer changes only after the delete: a refusal on a pinned element does not lose it
    if ok {
        publish(set_clip(clip));
        a.ed.selection.clear();
        a.view.e3.sel.clear();
    }
}

fn select_created(ed: &mut Editor, r: &PasteReport) {
    let sel: Vec<Sel> = r.created.iter().filter_map(|c| sel_of(&c.0, c.1)).collect();
    if !sel.is_empty() {
        ed.selection = sel;
    }
}

/// Paste as one undo step; replaying the operation reproduces the same ids.
fn commit(ed: &mut Editor, clip: Arc<Clip>, o: Option<PasteOpts>, levels: Vec<i64>) -> bool {
    commit_n(ed, clip, o, levels).is_some()
}

/// Same as [`commit`]; (created, skipped) on success.
fn commit_n(
    ed: &mut Editor,
    clip: Arc<Clip>,
    o: Option<PasteOpts>,
    levels: Vec<i64>,
) -> Option<(usize, usize)> {
    let out: Arc<Mutex<Option<PasteReport>>> = Arc::new(Mutex::new(None));
    let slot = out.clone();
    let res = ed.run_op(OpFn(Arc::new(move |m: &mut Model| {
        let r = match &o {
            Some(x) => m.clip_paste(&clip, x)?,
            None => m.clip_paste_levels(&clip, &levels)?,
        };
        if let Ok(mut g) = slot.lock() {
            *g = Some(r);
        }
        Ok(())
    })));
    match res {
        Ok(()) => {
            let r = out.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
            select_created(ed, &r);
            let mut msg = crate::tr(
                "clip.pasted",
                &[
                    ("n", &r.created.len().to_string()),
                    ("skipped", &r.skipped.len().to_string()),
                ],
            );
            if !r.materials_differ.is_empty() {
                msg.push_str(". ");
                msg.push_str(&crate::tr(
                    "clip.materials_differ",
                    &[("n", &r.materials_differ.len().to_string())],
                ));
            }
            ed.status = Some(msg);
            Some((r.created.len(), r.skipped.len()))
        }
        Err(e) => {
            report(ed, &e);
            None
        }
    }
}

/// Grid step of the 3D paste fallback, m.
const PASTE_STEP: f64 = 1.0;

/// Paste shift in 3D: to the cursor point when the copies then clear the originals, else
/// aside by whole grid steps. Returns the shift and whether it follows the cursor.
pub(crate) fn paste_shift3d(
    bbox: Option<(f64, f64, f64, f64)>,
    center: (f64, f64),
    cursor: Option<(f64, f64)>,
    step: f64,
) -> ((f64, f64), bool) {
    let Some((x0, y0, x1, y1)) = bbox else {
        return ((step, step), false);
    };
    if let Some(c) = cursor {
        let (dx, dy) = (c.0 - center.0, c.1 - center.1);
        let clear = x0 + dx >= x1 || x1 + dx <= x0 || y0 + dy >= y1 || y1 + dy <= y0;
        if clear {
            return ((dx, dy), true);
        }
    }
    let w = x1 - x0 + step;
    (((w / step).ceil() * step, 0.0), false)
}

fn paste(a: &mut EditorApp) {
    let Some(c) = clip_now() else {
        a.ed.status = Some(t("clip.clip_empty"));
        return;
    };
    // 3D: at the point under the cursor, else aside by whole grid steps; never on the originals
    if a.view3d {
        let cur = a.view.e3.cursor_pt.map(|p| (p.x as f64, p.y as f64));
        let ((dx, dy), at_cursor) = paste_shift3d(c.bbox, c.center(), cur, PASTE_STEP);
        let o = PasteOpts {
            dx,
            dy,
            level: PlaceLevel::Base(a.ed.level),
        };
        if let Some((n, skipped)) = commit_n(&mut a.ed, c, Some(o), vec![]) {
            // the paste report names what was skipped; keep it next to the placement note
            let rep = a.ed.status.take().filter(|_| skipped > 0);
            let key = if at_cursor {
                "edit3d.paste.placed"
            } else {
                "edit3d.paste.offset"
            };
            let s = crate::tr(key, &[("n", &n.to_string())]);
            a.ed.status = Some(rep.map_or(s.clone(), |r| format!("{s}; {r}")));
        }
        return;
    }
    a.ed.set_mode(Mode::Select);
    a.ed.status = Some(t("clip.paste_hint"));
    with(|s| s.armed = Some(Armed::Paste(c)));
}

fn paste_aligned(a: &mut EditorApp) {
    let Some(c) = clip_now() else {
        a.ed.status = Some(t("clip.clip_empty"));
        return;
    };
    let o = PasteOpts {
        dx: 0.0,
        dy: 0.0,
        level: PlaceLevel::Same,
    };
    commit(&mut a.ed, c, Some(o), vec![]);
}

fn level_rows(m: &Model) -> Vec<(i64, String, f64)> {
    let Ok(mut st) = m
        .sql()
        .prepare("SELECT id,name,elevation FROM level ORDER BY elevation DESC")
    else {
        return vec![];
    };
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, f64>(2)?,
        ))
    });
    let out = match rows {
        Ok(v) => v.filter_map(Result::ok).collect(),
        Err(_) => vec![],
    };
    out
}

fn paste_levels(a: &mut EditorApp) {
    if clip_now().is_none() {
        a.ed.status = Some(t("clip.clip_empty"));
        return;
    }
    let list = level_rows(&a.ed.model)
        .into_iter()
        .map(|(id, n, _)| (id, n, false))
        .collect();
    with(|s| s.dialog = Some(Dialog::Levels(list)));
}

fn match_cmd(a: &mut EditorApp) {
    let list = items(a);
    match list.as_slice() {
        [] => a.ed.status = Some(t("clip.match_need_sample")),
        [one] => {
            let sample = one.clone();
            a.ed.set_mode(Mode::Select);
            a.ed.status = Some(t("clip.match_hint"));
            with(|s| s.armed = Some(Armed::Match(sample)));
        }
        [first, rest @ ..] => {
            let (k, id) = first.clone();
            let targets = rest.to_vec();
            let n = targets.len().to_string();
            let done = run(&mut a.ed, "clip.match_done", move |m| {
                m.match_properties((k.as_str(), id), &refs(&targets))
                    .map(|_| ())
            });
            // the first selected is the source: the line says which one
            if done {
                let id = id.to_string();
                a.ed.status = Some(crate::tr("lnp.match_from", &[("id", &id), ("n", &n)]));
            }
        }
    }
}

fn need_two(a: &mut EditorApp) -> Option<Items> {
    let list = items(a);
    if list.len() < 2 {
        a.ed.status = Some(t("clip.join_need_two"));
        return None;
    }
    Some(list)
}

fn join(a: &mut EditorApp) {
    let Some(list) = need_two(a) else { return };
    let joined = a.ed.model.joins().unwrap_or_default();
    let inside = |p: &(String, i64)| list.contains(p);
    let any_free = {
        let n = joined
            .iter()
            .filter(|(x, y)| inside(x) && inside(y))
            .count();
        n == 0
    };
    if any_free {
        run(&mut a.ed, "clip.joined", move |m| {
            m.join_selected(&refs(&list)).map(|_| ())
        });
    } else {
        run(&mut a.ed, "clip.unjoined", move |m| {
            m.unjoin_selected(&refs(&list)).map(|_| ())
        });
    }
}

fn unjoin(a: &mut EditorApp) {
    let Some(list) = need_two(a) else { return };
    run(&mut a.ed, "clip.unjoined", move |m| {
        m.unjoin_selected(&refs(&list)).map(|_| ())
    });
}

fn switch_join(a: &mut EditorApp) {
    let Some(list) = need_two(a) else { return };
    run(&mut a.ed, "clip.join_switched", move |m| {
        m.switch_joins(&refs(&list)).map(|_| ())
    });
}

fn pin(a: &mut EditorApp) {
    let list = items(a);
    if list.is_empty() {
        a.ed.status = Some(t("clip.nothing_selected"));
        return;
    }
    let all = list
        .iter()
        .all(|x| a.ed.model.is_pinned(&x.0, x.1).unwrap_or(false));
    if all {
        run(&mut a.ed, "clip.unpinned", move |m| {
            m.unpin(&refs(&list)).map(|_| ())
        });
        return;
    }
    let (can, cannot): (Items, Items) = list.into_iter().partition(|x| a.ed.model.can_pin(&x.0));
    if can.is_empty() {
        a.ed.status = Some(t("clip.pin_none"));
        return;
    }
    let ok = run(&mut a.ed, "clip.pinned_done", move |m| {
        m.pin(&refs(&can)).map(|_| ())
    });
    if ok && !cannot.is_empty() {
        a.ed.status = Some(crate::tr(
            "clip.pinned_partial",
            &[("skipped", &cannot.len().to_string())],
        ));
    }
}

fn unpin(a: &mut EditorApp) {
    let list = items(a);
    if list.is_empty() {
        a.ed.status = Some(t("clip.nothing_selected"));
        return;
    }
    // only what is pinned: the model refuses to unpin a free element
    let pinned: Items = list
        .into_iter()
        .filter(|x| a.ed.model.is_pinned(&x.0, x.1).unwrap_or(false))
        .collect();
    if pinned.is_empty() {
        a.ed.status = Some(t("cmd_buttons.unpin.none"));
        return;
    }
    run(&mut a.ed, "clip.unpinned", move |m| {
        m.unpin(&refs(&pinned)).map(|_| ())
    });
}

/// Instances the selection belongs to.
fn instances(a: &EditorApp) -> Vec<i64> {
    let mut out = vec![];
    for (k, id) in items(a) {
        if let Ok(Some(i)) = a.ed.model.group_of(&k, id) {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    out
}

fn ungroup(a: &mut EditorApp) {
    let list = instances(a);
    if list.is_empty() {
        a.ed.status = Some(t("clip.no_group"));
        return;
    }
    run(&mut a.ed, "clip.ungrouped", move |m| {
        m.group_ungroup_many(&list)
    });
}

fn group_select(a: &mut EditorApp) {
    let list = expanded(a);
    let sel: Vec<Sel> = list.iter().filter_map(|x| sel_of(&x.0, x.1)).collect();
    if !sel.is_empty() {
        a.ed.selection = sel;
        a.ed.revision += 1;
    }
}

fn group_edit(a: &mut EditorApp) {
    let list = instances(a);
    let [inst] = list.as_slice() else {
        a.ed.status = Some(t("clip.one_group"));
        return;
    };
    let inst = *inst;
    with(|s| s.editing = Some((inst, vec![], vec![])));
    let members = a.ed.model.group_members(inst).unwrap_or_default();
    a.ed.selection = members.iter().filter_map(|x| sel_of(&x.0, x.1)).collect();
    a.ed.revision += 1;
    a.ed.status = Some(t("clip.edit_hint"));
}

fn group_add_member(a: &mut EditorApp, add: bool) {
    let list = items(a);
    if with(|s| s.editing.is_none()) {
        a.ed.status = Some(t("clip.not_editing"));
        return;
    }
    with(|s| {
        if let Some((_, adds, removes)) = &mut s.editing {
            for x in list {
                let (to, from) = if add {
                    (&mut *adds, &mut *removes)
                } else {
                    (&mut *removes, &mut *adds)
                };
                from.retain(|y| *y != x);
                if !to.contains(&x) {
                    to.push(x);
                }
            }
        }
    });
    a.ed.status = Some(t(if add {
        "clip.member_added"
    } else {
        "clip.member_removed"
    }));
}

fn group_finish(a: &mut EditorApp) {
    let Some((inst, adds, removes)) = with(|s| s.editing.take()) else {
        a.ed.status = Some(t("clip.not_editing"));
        return;
    };
    run(&mut a.ed, "clip.group_updated", move |m| {
        m.group_finish_edit(inst, &refs(&adds), &refs(&removes))
    });
}

/// Abandon the instance edit: the definition and other instances are unchanged.
fn group_cancel(a: &mut EditorApp) {
    if with(|s| s.editing.take()).is_none() {
        a.ed.status = Some(t("clip.not_editing"));
        return;
    }
    a.ed.status = Some(t("clip.group_cancelled"));
}

/// Deletes a definition with no instances; if instances exist, the model explains the refusal.
fn group_delete_cmd(a: &mut EditorApp) {
    let g = a.ed.model.groups().unwrap_or_default();
    if g.is_empty() {
        a.ed.status = Some(t("clip.no_groups"));
        return;
    }
    with(|s| s.dialog = Some(Dialog::GroupDelete(g)));
}

/// Default name "Group N" (localized), free in the model.
fn free_group_name(m: &Model) -> String {
    let have = m.groups().unwrap_or_default();
    let base = t("clip.group_default");
    let mut n = have.len() + 1;
    loop {
        let name = format!("{base} {n}");
        if !have.iter().any(|g| g.name == name) {
            return name;
        }
        n += 1;
    }
}

fn group_cmd(a: &mut EditorApp) {
    if items(a).is_empty() {
        a.ed.status = Some(t("clip.nothing_selected"));
        return;
    }
    let name = free_group_name(&a.ed.model);
    with(|s| s.dialog = Some(Dialog::GroupName(name)));
}

fn group_place_cmd(a: &mut EditorApp) {
    let g = a.ed.model.groups().unwrap_or_default();
    if g.is_empty() {
        a.ed.status = Some(t("clip.no_groups"));
        return;
    }
    with(|s| s.dialog = Some(Dialog::GroupPlace(g)));
}

fn arm_place(a: &mut EditorApp, group: i64) {
    let def: Option<String> =
        a.ed.model
            .sql()
            .query_row("SELECT def FROM el_group WHERE id=?1", [group], |r| {
                r.get(0)
            })
            .ok();
    let Some(c) = def.as_deref().and_then(Clip::from_text) else {
        a.ed.status = Some(t("clip.no_groups"));
        return;
    };
    a.ed.set_mode(Mode::Select);
    a.ed.status = Some(t("clip.place_hint"));
    with(|s| s.armed = Some(Armed::Place(group, Arc::new(c))));
}

pub fn register(app: &mut EditorApp) {
    app.register_command("edit.copy", |a| {
        copy(a);
    });
    app.register_command("edit.cut", cut);
    app.register_command("edit.paste", paste);
    app.register_command("edit.paste_aligned", paste_aligned);
    app.register_command("edit.paste_levels", paste_levels);
    app.register_command("mod.match", match_cmd);
    app.register_command("mod.join", join);
    app.register_command("mod.unjoin", unjoin);
    app.register_command("mod.switch_join", switch_join);
    app.register_command("mod.pin", pin);
    app.register_command("mod.unpin", unpin);
    app.register_command("mod.group", group_cmd);
    app.register_command("mod.ungroup", ungroup);
    app.register_command("mod.group_select", group_select);
    app.register_command("mod.group_edit", group_edit);
    app.register_command("mod.group_add", |a| group_add_member(a, true));
    app.register_command("mod.group_remove", |a| group_add_member(a, false));
    app.register_command("mod.group_finish", group_finish);
    app.register_command("mod.group_place", group_place_cmd);
    app.register_command("mod.group_delete", group_delete_cmd);
    app.register_command("mod.group_cancel", group_cancel);
}

/// Element under a plan point: the top candidate.
fn hit(ed: &Editor, p: V2) -> Option<(String, i64)> {
    let c = crate::selection::candidates(ed, p, ed.tool.radius, |_| true);
    c.first().and_then(|g| g.first()).and_then(|s| sel_item(*s))
}

/// Plan events. Motion is only recorded, so it does not interfere with hover and snaps.
pub fn handle(ed: &mut Editor, e: Event) -> bool {
    if let Event::Move(p) = e {
        with(|s| s.cursor = Some(p));
        return false;
    }
    if with(|s| s.armed.is_none()) {
        return false;
    }
    if !alive(ed) {
        reset();
        return false;
    }
    match e {
        Event::Key(Key::Escape) => {
            reset();
            ed.status = None;
            true
        }
        Event::Key(Key::Enter) => {
            let at = with(|s| s.cursor);
            match at {
                Some(p) if with(|s| matches!(s.armed, Some(Armed::Paste(_)))) => click(ed, p),
                _ => {
                    reset();
                    ed.status = None;
                }
            }
            true
        }
        Event::Click(p) => {
            click(ed, p);
            true
        }
        _ => false,
    }
}

fn click(ed: &mut Editor, p: V2) {
    let armed = with(|s| s.armed.take());
    match armed {
        Some(Armed::Paste(c)) => {
            let center = c.center();
            let o = PasteOpts {
                dx: p.x - center.0,
                dy: p.y - center.1,
                level: PlaceLevel::Base(ed.level),
            };
            commit(ed, c, Some(o), vec![]);
        }
        Some(Armed::Match(sample)) => {
            let target = hit(ed, p);
            if let Some(x) = target {
                let (k, id) = sample.clone();
                run(ed, "clip.match_done", move |m| {
                    m.match_properties((k.as_str(), id), &[(x.0.as_str(), x.1)])
                        .map(|_| ())
                });
            }
            // the sample is kept until Esc: matching usually targets several elements
            with(|s| s.armed = Some(Armed::Match(sample)));
        }
        Some(Armed::Place(group, c)) => {
            let level = ed.level;
            let (x, y) = (p.x, p.y);
            run(ed, "clip.placed", move |m| {
                m.group_place(group, level, x, y).map(|_| ())
            });
            with(|s| s.armed = Some(Armed::Place(group, c)));
        }
        None => {}
    }
}

/// Paste ghost at the cursor.
pub fn paint(p: &egui::Painter, t: &crate::ui_kit::Tokens, to_s: &dyn Fn(V2) -> egui::Pos2) {
    let (c, cur) = with(|s| {
        let c = match &s.armed {
            Some(Armed::Paste(c)) | Some(Armed::Place(_, c)) => Some(c.clone()),
            _ => None,
        };
        (c, s.cursor)
    });
    let (Some(c), Some(cur)) = (c, cur) else {
        return;
    };
    let center = c.center();
    let (dx, dy) = (cur.x - center.0, cur.y - center.1);
    let stroke = egui::Stroke::new(1.5, t.accent);
    for g in c.ghost.iter().step_by((c.ghost.len() / GHOST_PAINT).max(1)) {
        p.line_segment(
            [
                to_s(v2(g[0] + dx, g[1] + dy)),
                to_s(v2(g[2] + dx, g[3] + dy)),
            ],
            stroke,
        );
    }
}

/// Clipboard events (they do not arrive as keys), output to the system clipboard and windows. Every frame.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    project_changed(a);
    track_focus(ctx);
    if !ctx.wants_keyboard_input() || crate::schedule::ui::grid_focused(ctx) {
        let (events, shift) = ctx.input(|i| (i.events.clone(), i.modifiers.shift));
        let focus = with(|s| s.focus);
        let over = with(|s| s.dialog.is_some()) || over_window(ctx);
        for ev in events {
            match ev {
                egui::Event::Copy => match route(focus, over, ClipEv::Copy) {
                    Route::Model => {
                        copy(a);
                    }
                    Route::TableCopy => {
                        a.table_copy = true;
                        ctx.request_repaint();
                    }
                    Route::TablePaste | Route::Ignore => {}
                },
                egui::Event::Cut => {
                    if route(focus, over, ClipEv::Cut) == Route::Model {
                        cut(a);
                    }
                }
                egui::Event::Paste(text) => {
                    match route(focus, over, ClipEv::Paste) {
                        Route::Model => os_paste(a, &text, shift),
                        Route::TablePaste => {
                            a.table_paste = Some(text);
                            ctx.request_repaint();
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    let out = OUTBOX.lock().ok().and_then(|mut g| g.take());
    if let Some((gen, text)) = out {
        if gen == GEN.load(Ordering::SeqCst) {
            ctx.copy_text(text);
            with(|s| s.pending = false);
        }
    }
    if with(|s| s.pending) {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    if with(|s| matches!(s.armed, Some(Armed::Paste(_)) | Some(Armed::Place(..)))) {
        ctx.request_repaint();
    }
    dialogs(a, ctx);
    let (asked, rev) = (a.ed.pending_delete.is_some(), a.ed.revision);
    crate::editor::delete_deps::dialog(&mut a.ed, ctx);
    if asked && a.ed.revision != rev {
        crate::view_gfx_ui::prune_3d_sel(a);
    }
}

/// A different project: the click wait and instance edit referred to ids of the previous model.
fn project_changed(a: &EditorApp) {
    let path = a.ed.model.sql().path().unwrap_or("");
    let changed = with(|s| {
        let was = s.project.as_deref();
        let changed = was.is_some_and(|w| w != path);
        if was != Some(path) {
            s.project = Some(path.to_string());
        }
        changed
    });
    if changed {
        with(|s| {
            s.armed = None;
            s.editing = None;
        });
    }
}

/// A press in a table routes the clipboard to it; a press elsewhere in the window routes it back to the drawing.
fn track_focus(ctx: &egui::Context) {
    let press = ctx.input(|i| {
        if i.pointer.any_pressed() {
            i.pointer.press_origin()
        } else {
            None
        }
    });
    let rect = with(|s| s.table_rect.take());
    let over = press.is_some_and(|p| {
        ctx.layer_id_at(p)
            .is_some_and(|l| l.order != egui::Order::Background)
    });
    with(|s| {
        if let Some(p) = press.filter(|_| !over) {
            s.focus = if rect.is_some_and(|r| r.contains(p)) {
                Focus::Table
            } else {
                Focus::Canvas
            };
        }
        // the table was closed: focus has nowhere to stay
        if rect.is_none() {
            s.focus = Focus::Canvas;
        }
    });
}

/// Pointer over a popup, menu or list: the clipboard belongs to them, not the model.
fn over_window(ctx: &egui::Context) -> bool {
    let at = ctx.pointer_latest_pos().and_then(|p| ctx.layer_id_at(p));
    at.is_some_and(|l| l.order != egui::Order::Background)
}

/// Paste from the system clipboard. Foreign or corrupt text does not activate the old snapshot.
fn os_paste(a: &mut EditorApp, text: &str, shift: bool) {
    // our own text has not reached the system clipboard yet: it still holds the previous content
    if !with(|s| s.pending) {
        match incoming(text, &own_tag()) {
            Incoming::Use(c) => {
                set_clip(*c);
            }
            Incoming::Own => {}
            Incoming::TooBig => {
                a.ed.status = Some(t("clip.clip_too_big"));
                return;
            }
            Incoming::Foreign => {
                a.ed.status = Some(t("clip.clip_foreign"));
                return;
            }
            Incoming::Damaged => {
                a.ed.status = Some(t("clip.clip_bad"));
                return;
            }
        }
    }
    reset();
    if shift {
        paste_aligned(a);
    } else {
        paste(a);
    }
}

/// The window's one primary button; Enter runs it through the dialog scaffold.
fn primary(ui: &mut egui::Ui, label: &str) -> bool {
    crate::ui_kit::primary_action(ui, label, None)
}

fn dialogs(a: &mut EditorApp, ctx: &egui::Context) {
    let Some(mut d) = with(|s| s.dialog.take()) else {
        return;
    };
    let mut open = true;
    let mut accept = false;
    let mut chosen: Option<i64> = None;
    let delete = matches!(d, Dialog::GroupDelete(_));
    let title = match &d {
        Dialog::Levels(_) => t("cmd.edit.paste_levels"),
        Dialog::GroupName(_) => t("cmd.mod.group"),
        Dialog::GroupPlace(_) => t("cmd.mod.group_place"),
        Dialog::GroupDelete(_) => t("cmd.mod.group_delete"),
    };
    let class = if matches!(d, Dialog::GroupName(_)) {
        crate::ui_kit::DialogSize::Form
    } else {
        crate::ui_kit::DialogSize::Confirm
    };
    crate::ui_kit::Dialog::new("clip.dialog", title, class)
        .open(&mut open)
        .show(ctx, |ui| match &mut d {
            Dialog::Levels(list) => {
                ui.label(t("clip.levels_prompt"));
                for (_, name, on) in list.iter_mut() {
                    ui.checkbox(on, name.as_str());
                }
                ui.add_space(6.0);
                accept = primary(ui, &t("clip.paste_action"));
            }
            Dialog::GroupName(name) => {
                ui.label(t("clip.group_prompt"));
                let r = ui.text_edit_singleline(name);
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                ui.add_space(6.0);
                accept = primary(ui, &t("clip.group_action"));
            }
            Dialog::GroupPlace(list) | Dialog::GroupDelete(list) => {
                ui.label(t(if delete {
                    "clip.group_delete_prompt"
                } else {
                    "clip.place_prompt"
                }));
                for g in list.iter() {
                    let label = crate::tr(
                        "clip.group_row",
                        &[("name", g.name.as_str()), ("n", &g.instances.to_string())],
                    );
                    if ui.button(label).clicked() {
                        chosen = Some(g.id);
                    }
                }
            }
        });
    if !open {
        return;
    }
    if let Some(g) = chosen {
        if delete {
            run(&mut a.ed, "clip.group_deleted", move |m| m.group_delete(g));
        } else {
            arm_place(a, g);
        }
        return;
    }
    if !accept {
        with(|s| s.dialog = Some(d));
        return;
    }
    match d {
        Dialog::Levels(list) => {
            let levels: Vec<i64> = list.iter().filter(|x| x.2).map(|x| x.0).collect();
            match (clip_now(), levels.is_empty()) {
                (Some(c), false) => {
                    commit(&mut a.ed, c, None, levels);
                }
                _ => a.ed.status = Some(t("clip.levels_none")),
            }
        }
        Dialog::GroupName(name) => {
            let list = expanded(a);
            run(&mut a.ed, "clip.grouped", move |m| {
                m.group_create(&name, &refs(&list)).map(|_| ())
            });
        }
        Dialog::GroupPlace(_) | Dialog::GroupDelete(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::{LayerSpec, WallSpec};

    fn editor() -> (Editor, i64) {
        let mut ed = Editor::new_in_memory().unwrap();
        let lay = LayerSpec {
            material: "Concrete".into(),
            thickness: 0.2,
        };
        let wt = ed.model.add_wall_type("Test wall", &[lay]).unwrap();
        let level = ed.level;
        let w = ed
            .model
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .unwrap();
        ed.reload().unwrap();
        ed.set_mode(Mode::Select);
        (ed, w)
    }

    fn walls(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn paste3d_no_overlap() {
        let bb = Some((0.0, 0.0, 5.0, 0.2));
        let c = (2.5, 0.1);
        let over = |(dx, dy): (f64, f64)| {
            0.0 + dx < 5.0 && 5.0 + dx > 0.0 && 0.0 + dy < 0.2 && 0.2 + dy > 0.0
        };
        // no cursor, cursor on the original, cursor far away
        for cur in [None, Some((2.5, 0.1)), Some((2.5, 10.1))] {
            let (d, at) = paste_shift3d(bb, c, cur, PASTE_STEP);
            assert!(!over(d), "{cur:?}");
            assert_eq!(at, cur.is_some_and(|p| p.1 > 5.0));
        }
        let (ed_, w) = editor();
        let mut ed = ed_;
        let clip = Arc::new(ed.model.clip_copy(&[("wall", w)]).unwrap());
        let (d, _) = paste_shift3d(clip.bbox, clip.center(), None, PASTE_STEP);
        let o = PasteOpts {
            dx: d.0,
            dy: d.1,
            level: PlaceLevel::Base(ed.level),
        };
        assert_eq!(commit_n(&mut ed, clip, Some(o), vec![]), Some((1, 0)));
        assert_eq!(walls(&ed), 2);
    }

    #[test]
    fn commit_is_one_undo_step_and_selects_the_copy() {
        let (mut ed, w) = editor();
        let clip = Arc::new(ed.model.clip_copy(&[("wall", w)]).unwrap());
        let level = ed.level;
        let o = PasteOpts {
            dx: 0.0,
            dy: 4.0,
            level: PlaceLevel::Base(level),
        };
        assert!(commit(&mut ed, clip, Some(o), vec![]));
        assert_eq!(walls(&ed), 2);
        assert_eq!(ed.selection.len(), 1);
        assert_ne!(ed.selection[0], Sel::Wall(w));
        ed.undo();
        assert_eq!(walls(&ed), 1);
        ed.redo();
        assert_eq!(walls(&ed), 2);
    }

    #[test]
    fn selection_kinds_map_to_tables_and_back() {
        for (k, id) in [
            ("wall", 1),
            ("opening", 2),
            ("slab", 3),
            ("comp", 4),
            ("col", 5),
            ("beam", 6),
            ("mesh", 7),
        ] {
            let s = sel_of(k, id).unwrap();
            assert_eq!(sel_item(s), Some((k.to_string(), id)));
        }
        assert_eq!(
            sel_item(Sel::Panel(9, 2)),
            Some(("curtain_wall".to_string(), 9))
        );
    }

    #[test]
    fn armed_paste_commits_on_click_and_cancels_on_escape() {
        let (mut ed, w) = editor();
        let clip = Arc::new(ed.model.clip_copy(&[("wall", w)]).unwrap());
        with(|s| s.armed = Some(Armed::Paste(clip.clone())));
        assert!(handle(&mut ed, Event::Key(Key::Escape)));
        assert!(with(|s| s.armed.is_none()));
        assert_eq!(walls(&ed), 1);
        with(|s| s.armed = Some(Armed::Paste(clip)));
        assert!(!handle(&mut ed, Event::Move(v2(9.0, 9.0))));
        assert!(handle(&mut ed, Event::Click(v2(2.5, 6.0))));
        assert_eq!(walls(&ed), 2);
        let y: f64 = ed
            .model
            .sql()
            .query_row("SELECT ay FROM wall ORDER BY id DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            (y - 6.0).abs() < 1e-9,
            "center of the copy lands under the cursor"
        );
        assert!(with(|s| s.armed.is_none()));
    }

    #[test]
    fn clipboard_events_follow_the_focused_area() {
        assert_eq!(route(Focus::Canvas, false, ClipEv::Cut), Route::Model);
        assert_eq!(route(Focus::Canvas, false, ClipEv::Paste), Route::Model);
        assert_eq!(route(Focus::Table, false, ClipEv::Copy), Route::TableCopy);
        assert_eq!(route(Focus::Table, false, ClipEv::Cut), Route::Ignore);
        assert_eq!(route(Focus::Table, false, ClipEv::Paste), Route::TablePaste);
        for ev in [ClipEv::Copy, ClipEv::Cut, ClipEv::Paste] {
            assert_eq!(route(Focus::Canvas, true, ev), Route::Ignore);
        }
    }

    #[test]
    fn clipboard_text_is_classified_before_use() {
        let (ed, w) = editor();
        let clip = ed.model.clip_copy(&[("wall", w)]).unwrap();
        assert!(matches!(incoming(&clip.to_text(), "7-"), Incoming::Use(_)));
        assert_eq!(incoming("plain text", "7-"), Incoming::Foreign);
        assert_eq!(incoming(&Clip::marker("7-3"), "7-"), Incoming::Own);
        assert_eq!(incoming(&Clip::marker("8-3"), "7-"), Incoming::TooBig);
        assert_eq!(incoming(&Clip::marker(""), "7-"), Incoming::TooBig);
        assert_eq!(
            incoming("DDC-CLIP/1\n{\"tables\":", "7-"),
            Incoming::Damaged
        );
    }

    #[test]
    fn nothing_armed_passes_events_through() {
        let (mut ed, _) = editor();
        reset();
        assert!(!handle(&mut ed, Event::Click(v2(0.0, 0.0))));
    }

    #[test]
    fn match_of_two_or_more_names_the_first_as_source() {
        let (mut ed, w) = editor();
        let spec = ed.walls.iter().find(|x| x.id == w).map(|x| x.wall_type).unwrap();
        let w2 = ed
            .model
            .add_wall(&WallSpec {
                a: (0.0, 3.0),
                b: (5.0, 3.0),
                level: ed.level,
                base_offset: 0.0,
                height: 2.5,
                wall_type: spec,
                offset0: Some(0.0),
            })
            .unwrap();
        ed.reload().unwrap();
        let mut a = EditorApp::new(ed, None, "test".into());
        a.ed.selection = vec![Sel::Wall(w), Sel::Wall(w2)];
        reset();
        match_cmd(&mut a);
        let want = crate::tr("lnp.match_from", &[("id", &w.to_string()), ("n", "1")]);
        assert_eq!(a.ed.status.as_deref(), Some(want.as_str()));
        assert!(crate::tr_str(true, "lnp.match_from").contains("{id}"));
        assert!(crate::tr_str(false, "lnp.match_from").contains("{n}"));
    }
}
