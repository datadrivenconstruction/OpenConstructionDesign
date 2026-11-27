// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project browser (TOOLS_UI §5): groups "Grids and reference planes", "Families"
//! (Category → Family → Type), "Systems" (network kind → system → segments), "Rooms",
//! tree search. Also the state of the browser windows and `register()` of their commands.
//! Right-click on a type: "Select all instances" (in view / in project), "Replace type…" (`replace_type`).
use super::library_win::{self, Library};
use super::param_editor::{self, Draft};
use super::prop_rows::{self, Cached};
use super::replace_type;
use super::type_dialog::{self, Fam, TypeDraft};
use super::EditorApp;
use crate::editor::{Editor, Sel};
use crate::units::Units;
use ddc_model::Model;
use ddc_ui::theme::Role;
use ddc_ui::{Catalog, Lang, Theme};
use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Windows and buffers of the project browser (UI thread).
#[derive(Default)]
pub struct LaneC {
    pub type_dlg: Option<TypeDraft>,
    pub params: Option<Draft>,
    pub library: Option<Library>,
    pub query: String,
    /// Multi-selection category in properties.
    pub multi_cat: Option<&'static str>,
    /// Multi-selection height buffer: (id set, text).
    pub multi_h: Option<(Vec<i64>, String)>,
    pub multi_o: Option<(Vec<i64>, [String; 3])>,
    /// Expand tree groups (demo/search).
    pub expand: bool,
    /// Tree nodes expanded by the user.
    open: HashSet<u64>,
    tree: Option<Tree>,
    nav: Nav,
    pub wall_vals: Cached<(u64, Units), prop_rows::WallVals>,
    pub opening_vals: Cached<(u64, Units), prop_rows::OpeningVals>,
    demo: Option<String>,
    /// `drafts` answer by (model address, `Editor::cache_key`) and the two drafts it was read for.
    drafts_memo: Option<DraftsMemo>,
    /// Library entries by (model address, `Editor::cache_key`, user folder stamp).
    lib_memo: Option<(LibKey, std::rc::Rc<Vec<crate::complib::Entry>>)>,
}

type LibKey = (usize, (u64, u64), Option<web_time::SystemTime>);

/// Entries of the library window, gathered again only when the model or the user folder changed.
fn library_entries(ed: &Editor) -> std::rc::Rc<Vec<crate::complib::Entry>> {
    let dir = crate::complib::user_dir();
    let stamp = dir
        .as_deref()
        .and_then(|d| std::fs::metadata(d).and_then(|m| ddc_model::wtime::modified(&m)).ok());
    let key = (&ed.model as *const Model as usize, ed.cache_key(), stamp);
    if let Some(v) = with(|s| s.lib_memo.as_ref().filter(|m| m.0 == key).map(|m| m.1.clone())) {
        return v;
    }
    let (all, bad) = crate::complib::gather(&ed.model, dir.as_deref());
    for b in bad {
        eprintln!("component library: {b} is not readable");
    }
    let v = std::rc::Rc::new(all);
    with(|s| s.lib_memo = Some((key, v.clone())));
    v
}

/// The drafts are compared by value each frame and copied only when one changed.
struct DraftsMemo {
    key: (usize, (u64, u64)),
    ty: Option<TypeDraft>,
    params: Option<Draft>,
    answer: (Option<String>, Option<String>),
}

thread_local! {
    static STATE: RefCell<LaneC> = RefCell::new(LaneC::default());
}

pub fn with<R>(f: impl FnOnce(&mut LaneC) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn open_types(app: &mut EditorApp) {
    let u = app.ed.tool.units;
    let m = &app.ed.model;
    let d = match app.ed.selection.first() {
        Some(Sel::Wall(id)) => app
            .ed
            .walls
            .iter()
            .find(|w| w.id == *id)
            .map(|w| TypeDraft::load(m, Fam::Wall, w.wall_type, u)),
        Some(Sel::Slab(id)) => m
            .sql()
            .query_row("SELECT stype FROM slab WHERE id=?1", [*id], |r| r.get(0))
            .ok()
            .map(|t| TypeDraft::load(m, Fam::Slab, t, u)),
        _ => None,
    }
    .unwrap_or_else(|| TypeDraft::first(m, Fam::Wall, u));
    let mut d = d;
    d.structure = d.fam.layered();
    with(|s| type_dialog::open_in(&mut s.type_dlg, d, m));
}

/// `mod.change_type`: the selected walls take another type (one undo step); the type editor
/// of `mgr.types` stays a separate command.
fn change_type(app: &mut EditorApp) {
    if super::type_library::selected_walls(&app.ed).is_empty() {
        app.ed.status = Some(app.sh.cat.t("cmd_buttons.change_type.none"));
        return;
    }
    super::type_library::open_swap();
}

fn open_params(app: &mut EditorApp) {
    let m = &app.ed.model;
    let fam = m
        .families()
        .ok()
        .and_then(|v| v.first().and_then(|(id, _)| m.family(*id).ok()))
        .or_else(|| ddc_model::library().ok().and_then(|v| v.into_iter().next()));
    if let Some(f) = fam {
        with(|s| s.params = Some(Draft::from_family(&f)));
    }
}

fn open_library(_: &mut EditorApp) {
    open_library_window();
}

/// Opens the component library window (search cleared, source and pick kept).
pub fn open_library_window() {
    with(|s| s.library.get_or_insert_with(Library::default).query.clear());
}

/// Commands of the project browser (their ids are in `commands.json`).
pub fn register(app: &mut EditorApp) {
    app.register_command("mgr.types", open_types);
    app.register_command("mod.change_type", change_type);
    app.register_command("mgr.params", open_params);
    app.register_command("mgr.library", open_library);
    app.register_command("mgr.load", open_library);
}

pub fn ru(app: &EditorApp) -> bool {
    app.sh.cat.lang == ddc_ui::Lang::Ru
}

/// Project browser windows; called every frame from `panel_docks`.
pub fn windows(ctx: &egui::Context, app: &mut EditorApp) {
    demo(app);
    let ru = ru(app);
    let u = app.ed.tool.units;
    replace_type::show(ctx, &app.sh.theme, ru, &mut app.ed);
    super::type_library::windows(ctx, app);
    if let Some(mut d) = with(|s| s.type_dlg.take()) {
        if type_dialog::window(ctx, &app.sh.theme, &app.sh.cat, ru, &mut app.ed, &mut d) {
            with(|s| s.type_dlg = Some(d));
        }
    }
    if let Some(mut lib) = with(|s| s.library.take()) {
        let entries = library_entries(&app.ed);
        let mut edit = None;
        let keep = library_win::show(ctx, &app.sh.theme, &app.sh.cat, &entries, &mut lib, &mut edit);
        if let Some((name, def)) = lib.load.take() {
            load_family_def(&mut app.ed, &name, def, &app.sh.cat);
        }
        if let Some(p) = lib.place.take() {
            crate::tools::component::arm_with(&mut app.ed, &p, app.sh.cat.lang);
            app.ed.status = Some(app.sh.cat.tf("complib.armed", &[("name", p.label().as_str())]));
        }
        if std::mem::take(&mut lib.open_dir) {
            if let Some(d) = crate::complib::user_dir() {
                match std::fs::create_dir_all(&d) {
                    Ok(()) => crate::gfx::open_path(&d),
                    Err(e) => app.ed.status = Some(e.to_string()),
                }
            }
        }
        let cmd = lib.cmd.take();
        with(|s| {
            if keep {
                s.library = Some(lib);
            }
            if edit.is_some() {
                s.params = edit;
            }
        });
        if let Some(c) = cmd {
            app.run(c);
        }
    }
    if let Some(mut d) = with(|s| s.params.take()) {
        match param_editor::window(ctx, &app.sh.theme, u, &mut d) {
            None => with(|s| s.params = Some(d)),
            Some(param_editor::Outcome::Ok) => {
                if !save_params(&mut app.ed, &d, &app.sh.cat) {
                    with(|s| s.params = Some(d));
                }
            }
            Some(param_editor::Outcome::Cancel) => {}
        }
    }
}

/// "Load into project" from the library window: a family from its definition (built-in or from the
/// user's folder) in a single operation.
pub fn load_family_def(ed: &mut crate::editor::Editor, name: &str, def: std::sync::Arc<str>, c: &Catalog) -> bool {
    let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.load_family(&def).map(|_| ())
    }));
    let r = ed.run_op(f);
    ed.status = Some(match &r {
        Ok(()) => c.tf("browser.family_loaded", &[("name", name)]),
        Err(e) => c.tf(
            "browser.family_load_failed",
            &[("name", name), ("e", e.to_string().as_str())],
        ),
    });
    r.is_ok()
}

/// "Load into project": a library family in a single operation (one undo step).
pub fn load_family(ed: &mut crate::editor::Editor, name: &str, c: &Catalog) -> bool {
    let n = name.to_string();
    let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.load_library_family(&n).map(|_| ())
    }));
    let r = ed.run_op(f);
    ed.status = Some(match &r {
        Ok(()) => c.tf("browser.family_loaded", &[("name", name)]),
        Err(e) => c.tf(
            "browser.family_load_failed",
            &[("name", name), ("e", e.to_string().as_str())],
        ),
    });
    r.is_ok()
}

/// Parameter editor OK: type-size values of a project family in a single operation.
/// A family that exists only in the library must first be loaded into the project. `false` keeps the window open.
pub fn save_params(ed: &mut crate::editor::Editor, d: &Draft, c: &Catalog) -> bool {
    let fid = match ed.model.family_id(&d.family) {
        Ok(Some(id)) => id,
        _ => {
            ed.status = Some(c.tf("browser.family_missing", &[("name", d.family.as_str())]));
            return false;
        }
    };
    let types = match d.type_values() {
        Ok(t) => t,
        Err(e) => {
            ed.status = Some(e);
            return false;
        }
    };
    let f = crate::editor::OpFn(std::sync::Arc::new(move |m: &mut ddc_model::Model| {
        m.set_comp_types(fid, &types)
    }));
    match ed.run_op(f) {
        Ok(()) => {
            ed.status = Some(c.tf("browser.params_saved", &[("name", d.family.as_str())]));
            true
        }
        Err(e) => {
            ed.status = Some(e.to_string());
            false
        }
    }
}

/// The parameter editor differs from the family it shows (the project's, else the library's).
/// A family found in neither counts as edited, so the question still asks.
fn params_dirty(m: &Model, d: &Draft) -> bool {
    let base = match m.family_id(&d.family) {
        Ok(Some(id)) => m.family(id).ok(),
        _ => ddc_model::library()
            .ok()
            .and_then(|v| v.into_iter().find(|f| f.name == d.family)),
    };
    base.is_none_or(|f| {
        let b = Draft::from_family(&f);
        b.rows != d.rows || b.types != d.types
    })
}

/// Drafts a project close or swap would lose: the type dialog (the stored type name) and the
/// parameter editor (the family name).
pub(crate) fn drafts(m: &Model) -> (Option<String>, Option<String>) {
    STATE.with(|s| {
        let Ok(s) = s.try_borrow() else {
            return (None, None);
        };
        let ty = s
            .type_dlg
            .as_ref()
            .filter(|d| d.dirty(m))
            .map(|d| d.stored_name().to_string());
        let params = s
            .params
            .as_ref()
            .filter(|d| params_dirty(m, d))
            .map(|d| d.family.clone());
        (ty, params)
    })
}

#[cfg(test)]
thread_local! {
    static DRAFT_QUERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// `drafts` for the editor's model, remembered per `Editor::cache_key` (an older revision put
/// back by a level walk still reads anew) and per draft value: the title and close checks ask
/// every frame, and the dirty test reads the model.
pub(crate) fn drafts_at(ed: &Editor) -> (Option<String>, Option<String>) {
    let m = &ed.model;
    let key = (m as *const Model as usize, ed.cache_key());
    let hit = STATE.with(|s| {
        let s = s.try_borrow().ok()?;
        let h = s.drafts_memo.as_ref()?;
        (h.key == key && h.ty == s.type_dlg && h.params == s.params).then(|| h.answer.clone())
    });
    if let Some(v) = hit {
        return v;
    }
    #[cfg(test)]
    DRAFT_QUERIES.with(|q| q.set(q.get() + 1));
    let v = drafts(m);
    STATE.with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            let (ty, params) = (s.type_dlg.clone(), s.params.clone());
            s.drafts_memo = Some(DraftsMemo { key, ty, params, answer: v.clone() });
        }
    });
    v
}

/// New, Open, import or restore replaced the model: a type dialog or parameter editor of the
/// old project must not show up over the new one.
pub(crate) fn on_model_reset() {
    with(|s| {
        s.type_dlg = None;
        s.params = None;
        s.multi_h = None;
        s.multi_o = None;
        s.drafts_memo = None;
    });
}

/// Discard on the project question: both windows close without writing, as Cancel does.
pub(crate) fn discard_drafts() {
    with(|s| {
        s.type_dlg = None;
        s.params = None;
    });
}

/// Save on the project question: Apply of the type dialog, then OK of the parameter editor.
/// A refusal stops there; that window stays open with its edits and reason.
pub(crate) fn save_drafts(ed: &mut Editor, c: &Catalog) -> bool {
    let (mut ty, mut params) = with(|s| (s.type_dlg.take(), s.params.take()));
    let mut ok = match ty.as_mut() {
        Some(d) if d.dirty(&ed.model) => type_dialog::apply(ed, d, c.lang == Lang::Ru),
        _ => true,
    };
    if ok {
        // the asked action swaps or closes the project: an open dialog would show a stale type
        ty = None;
    }
    if ok && params.as_ref().is_some_and(|d| params_dirty(&ed.model, d)) {
        ok = params.as_ref().is_some_and(|d| save_params(ed, d, c));
        if ok {
            params = None;
        }
    }
    with(|s| {
        s.type_dlg = ty;
        s.params = params;
    });
    ok
}

/// Tree node action.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    None,
    /// Muted placeholder under a family group without types.
    Empty,
    Type(Fam, i64),
    /// Component type: family, type.
    Comp(i64, i64),
    System(i64),
    /// Element and the level on which it is visible.
    Select(Sel, Option<i64>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub label: String,
    pub right: String,
    pub act: Act,
    pub kids: Vec<Node>,
}

fn node(label: impl Into<String>, right: impl Into<String>, act: Act, kids: Vec<Node>) -> Node {
    Node {
        label: label.into(),
        right: right.into(),
        act,
        kids,
    }
}

fn count(m: &Model, q: &str, id: i64) -> i64 {
    m.sql().query_row(q, [id], |r| r.get(0)).unwrap_or(0)
}

fn rows2(m: &Model, q: &str) -> Vec<(i64, String)> {
    let Ok(mut st) = m.sql().prepare(q) else {
        return vec![];
    };
    st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
}

/// A family group is always a branch: without types it holds one muted placeholder.
fn or_empty(c: &Catalog, kids: Vec<Node>) -> Vec<Node> {
    if kids.is_empty() {
        vec![node(c.t("browser.fam.empty"), "", Act::Empty, vec![])]
    } else {
        kids
    }
}

/// "Families": the families the project uses (system types with instances, members of every
/// structural kind, trusses, script parts by IFC class) and the loaded components. A new project
/// lists none.
pub fn families(m: &Model, c: &Catalog) -> Vec<Node> {
    let ru = c.lang == Lang::Ru;
    let used = |k: i64| (k > 0).then(|| k.to_string());
    let sys = |fam: Fam, q: &str| -> Option<Node> {
        let kids: Vec<Node> = type_dialog::types_of(m, fam)
            .into_iter()
            .filter_map(|(id, n)| used(count(m, q, id)).map(|k| node(n, k, Act::Type(fam, id), vec![])))
            .collect();
        (!kids.is_empty()).then(|| node(fam.label(ru), "", Act::None, kids))
    };
    // a member type is used by columns, beams, braces, joists and truss chords and webs
    let member_use = "SELECT (SELECT count(*) FROM col WHERE mtype=?1) + (SELECT count(*) FROM beam WHERE mtype=?1)          + (SELECT count(*) FROM brace WHERE mtype=?1) + (SELECT count(*) FROM beam_system WHERE mtype=?1)          + (SELECT count(*) FROM truss WHERE chord_type=?1 OR web_type=?1)";
    let mut out: Vec<Node> = [
        sys(Fam::Wall, "SELECT count(*) FROM wall WHERE wtype=?1"),
        sys(Fam::Slab, "SELECT count(*) FROM slab WHERE stype=?1"),
        sys(Fam::Member, member_use),
    ]
    .into_iter()
    .flatten()
    .collect();
    let grouped = |q: &str| -> Vec<Node> {
        rows2(m, q)
            .into_iter()
            .map(|(n, name)| node(name, n.to_string(), Act::None, vec![]))
            .collect()
    };
    let trusses: Vec<Node> = grouped("SELECT count(*), web FROM truss GROUP BY web ORDER BY web")
        .into_iter()
        .map(|mut n| {
            let k = format!("opt.web.{}", n.label);
            if c.table(Lang::En).contains_key(&k) {
                n.label = c.t(&k);
            }
            n
        })
        .collect();
    if !trusses.is_empty() {
        out.push(node(c.t("type_dialog.warn.cat.truss"), "", Act::None, trusses));
    }
    // parametric parts (script members, plates) by their IFC class, then by name
    let known = ddc_model::SOLID_CATEGORIES.iter().map(|r| format!("'{}'", r.0)).collect::<Vec<_>>().join(",");
    let by_class = ddc_model::SOLID_CATEGORIES
        .iter()
        .map(|&(class, key)| (format!("ifc_class='{class}'"), key))
        .chain([(format!("coalesce(ifc_class,'') NOT IN ({known})"), "solid")]);
    for (cond, key) in by_class {
        let kids = grouped(&format!(
            "SELECT count(*), CASE WHEN name='' THEN coalesce(ifc_type,'') ELSE name END AS n FROM solid              WHERE void=0 AND {cond} GROUP BY n ORDER BY n"
        ));
        if kids.is_empty() {
            continue;
        }
        let label = c.t(&format!("type_dialog.warn.cat.{key}"));
        match out.iter_mut().find(|n| n.label == label) {
            Some(n) => n.kids.extend(kids),
            None => out.push(node(label, "", Act::None, kids)),
        }
    }
    let mut cats: Vec<Node> = vec![];
    let fams: Vec<(i64, String, String)> = {
        let Ok(mut st) = m
            .sql()
            .prepare("SELECT id, name, category FROM comp_family ORDER BY category, name")
        else {
            return out;
        };
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    };
    for (fid, name, cat) in fams {
        let types = m.comp_types(fid).unwrap_or_default();
        let kids: Vec<Node> = types
            .into_iter()
            .map(|(tid, tn)| {
                let k = count(m, "SELECT count(*) FROM comp WHERE ctype=?1", tid);
                node(
                    tn,
                    if k > 0 { k.to_string() } else { String::new() },
                    Act::Comp(fid, tid),
                    vec![],
                )
            })
            .collect();
        let label = library_win::category_label(c, &cat);
        let f = node(name, "", Act::Comp(fid, 0), or_empty(c, kids));
        match cats.iter_mut().find(|n| n.label == label) {
            Some(n) => n.kids.push(f),
            None => cats.push(node(label, "", Act::None, vec![f])),
        }
    }
    out.extend(cats);
    out
}

fn domain_group(kind: &str) -> &'static str {
    match kind {
        "supply_air" | "return_air" | "exhaust_air" => "browser.sys.air",
        "cold_water" | "hot_water" | "heating" | "chilled_water" | "sprinkler" | "medical_gas" => "browser.sys.water",
        "sanitary" => "browser.sys.sanitary",
        _ => "browser.sys.power",
    }
}

/// "Systems": network kind → system → segments by size.
pub fn systems(m: &Model, c: &Catalog) -> Vec<Node> {
    let mut out: Vec<Node> = vec![];
    for s in m.mep_systems().unwrap_or_default() {
        let g = c.t(domain_group(s.kind.name()));
        let Ok(mut st) = m.sql().prepare(
            "SELECT domain, shape, w, h, count(*) FROM mep_run WHERE system=?1 GROUP BY domain, shape, w, h",
        ) else {
            continue;
        };
        let kids: Vec<Node> = st
            .query_map([s.id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map(|it| it.flatten().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|(d, sh, w, h, n)| {
                let size = if sh == "round" {
                    format!("Ø{:.0}", w * 1000.0)
                } else {
                    format!("{:.0}×{:.0}", w * 1000.0, h * 1000.0)
                };
                let what = c.t(match d.as_str() {
                    "duct" => "browser.run.duct",
                    "cable_tray" => "browser.run.tray",
                    "conduit" => "browser.run.conduit",
                    _ => "browser.run.pipe",
                });
                node(
                    format!("{what} {size}"),
                    n.to_string(),
                    Act::System(s.id),
                    vec![],
                )
            })
            .collect();
        let total: usize = kids.len();
        let sys = node(
            s.name.clone(),
            if total > 0 {
                total.to_string()
            } else {
                String::new()
            },
            Act::System(s.id),
            kids,
        );
        match out.iter_mut().find(|n| n.label == g) {
            Some(n) => n.kids.push(sys),
            None => out.push(node(g, "", Act::None, vec![sys])),
        }
    }
    out
}

/// Kernel element row: selection and the level where it is visible (a zone uses the level of its first room).
pub(crate) fn core_act(ed: &Editor, kind: &str, id: i64) -> Act {
    let level: Option<i64> = match kind {
        "ramp" => ed
            .model
            .sql()
            .query_row("SELECT level FROM ramp WHERE id=?1", [id], |r| r.get(0))
            .ok(),
        "shaft" => ed.model.shaft(id).ok().map(|s| s.level),
        "zone" => ed.model.zone(id).ok().and_then(|z| {
            let room = *z.rooms.first()?;
            ed.model
                .sql()
                .query_row("SELECT level FROM room WHERE id=?1", [room], |r| r.get(0))
                .ok()
        }),
        // a grid line is shown on the levels of its range: go to the first one when hidden here
        "grid" => ed.model.grid(id).ok().and_then(|g| {
            let shown = ed
                .model
                .grid_axes(Some(ed.level))
                .is_ok_and(|a| a.iter().any(|x| x.label == g.name));
            if shown {
                Some(ed.level)
            } else {
                g.spec.base_level.or(Some(ed.level))
            }
        }),
        _ => Some(ed.level),
    };
    crate::editor::structx::sel(kind, id).map_or(Act::None, |s| Act::Select(s, level))
}

/// Select the row's element, switching to its level.
pub(crate) fn select_row(ed: &mut Editor, act: Act) {
    if let Act::Select(s, level) = act {
        if let Some(l) = level {
            ed.set_level(l);
        }
        ed.selection = vec![s];
    }
}

/// Project ramps and zones, separators of the current level.
fn core_elements(ed: &Editor) -> Vec<Node> {
    use crate::tr;
    let m = &ed.model;
    let mut out = vec![];
    let ramps = m.all_ramps().unwrap_or_default();
    if !ramps.is_empty() {
        out.push(node(
            tr("selcat.ramp", &[]),
            ramps.len().to_string(),
            Act::None,
            ramps
                .iter()
                .map(|(id, _)| {
                    let label = tr("core_tools.browser.ramp", &[("id", id.to_string().as_str())]);
                    let broken = matches!(m.ramp_problem(*id), Ok(Some(_)));
                    node(
                        label,
                        if broken { "⚠" } else { "" },
                        core_act(ed, "ramp", *id),
                        vec![],
                    )
                })
                .collect(),
        ));
    }
    let zones = m.zones().unwrap_or_default();
    if !zones.is_empty() {
        out.push(node(
            tr("selcat.zone", &[]),
            zones.len().to_string(),
            Act::None,
            zones
                .into_iter()
                .map(|(id, z)| {
                    node(
                        z.name,
                        z.rooms.len().to_string(),
                        core_act(ed, "zone", id),
                        vec![],
                    )
                })
                .collect(),
        ));
    }
    let shafts = m.shaft_ids().unwrap_or_default();
    if !shafts.is_empty() {
        out.push(node(
            tr("selcat.shaft", &[]),
            shafts.len().to_string(),
            Act::None,
            shafts
                .iter()
                .map(|id| {
                    let label = tr("shaft_tool.browser.shaft", &[("id", id.to_string().as_str())]);
                    node(label, "", core_act(ed, "shaft", *id), vec![])
                })
                .collect(),
        ));
    }
    let seps = m.room_seps(ed.level).unwrap_or_default();
    if !seps.is_empty() {
        out.push(node(
            tr("selcat.room_sep", &[]),
            seps.len().to_string(),
            Act::None,
            seps.iter()
                .map(|(id, _)| {
                    let label = tr("core_tools.browser.sep", &[("id", id.to_string().as_str())]);
                    node(label, "", core_act(ed, "room_sep", *id), vec![])
                })
                .collect(),
        ));
    }
    out
}

/// "Grids and reference planes".
pub fn datums(ed: &Editor, c: &Catalog) -> Vec<Node> {
    let m = &ed.model;
    let g = rows2(m, "SELECT id, name FROM grid ORDER BY name");
    let r = rows2(m, "SELECT id, name FROM ref_plane ORDER BY id");
    let mut out = vec![];
    if !g.is_empty() {
        out.push(node(
            c.t("browser.grids"),
            g.len().to_string(),
            Act::None,
            g.into_iter()
                .map(|(id, n)| node(n, "", core_act(ed, "grid", id), vec![]))
                .collect(),
        ));
    }
    if !r.is_empty() {
        out.push(node(
            c.t("browser.ref_planes"),
            r.len().to_string(),
            Act::None,
            r.into_iter()
                .map(|(id, n)| {
                    node(
                        if n.is_empty() { format!("#{id}") } else { n },
                        "",
                        core_act(ed, "ref_plane", id),
                        vec![],
                    )
                })
                .collect(),
        ));
    }
    out
}

/// Search: a node stays if its own name matches (with its subtree) or a descendant's name matches.
pub fn filter(nodes: &[Node], q: &str) -> Vec<Node> {
    let q = q.trim().to_lowercase();
    if q.is_empty() {
        return nodes.to_vec();
    }
    nodes
        .iter()
        .filter_map(|n| {
            if n.label.to_lowercase().contains(&q) {
                return Some(n.clone());
            }
            let kids = filter(&n.kids, &q);
            (!kids.is_empty()).then(|| Node { kids, ..n.clone() })
        })
        .collect()
}

pub(crate) const ROW_H: f32 = 24.0;
const INDENT: f32 = 14.0;

/// Tree node in traversal order; `span` covers the node with its subtree.
struct Flat {
    depth: u16,
    span: u32,
    key: u64,
    label: String,
    right: String,
    act: Act,
}

fn flatten(nodes: Vec<Node>, depth: u16, parent: u64, out: &mut Vec<Flat>) {
    for n in nodes {
        let mut h = DefaultHasher::new();
        (parent, &n.label).hash(&mut h);
        let key = h.finish();
        let at = out.len();
        out.push(Flat {
            depth,
            span: 1,
            key,
            label: n.label,
            right: n.right,
            act: n.act,
        });
        flatten(n.kids, depth + 1, key, out);
        out[at].span = (out.len() - at) as u32;
    }
}

/// Currently visible rows: a collapsed branch skips its subtree.
fn visible(rows: &[Flat], open: &HashSet<u64>, all: bool) -> Vec<u32> {
    let mut out = Vec::with_capacity(rows.len().min(1024));
    let mut i = 0;
    while i < rows.len() {
        out.push(i as u32);
        let r = &rows[i];
        i += if r.span > 1 && !all && !open.contains(&r.key) {
            r.span as usize
        } else {
            1
        };
    }
    out
}

/// Which of the `n` rows below `top` fall into the clip band.
fn row_window(top: f32, clip: (f32, f32), n: usize) -> std::ops::Range<usize> {
    let from = ((clip.0 - top) / ROW_H).floor().max(0.0) as usize;
    let to = (((clip.1 - top) / ROW_H).ceil().max(0.0) as usize).min(n);
    from.min(to)..to
}

struct Group {
    title: String,
    icon: &'static str,
    rows: Vec<Flat>,
    shown: Vec<u32>,
}

type TreeKey = (u64, i64, usize, Lang);

/// Frame tree: SQL and filtering run only when the model or the query changes.
struct Tree {
    key: TreeKey,
    query: String,
    force: bool,
    groups: Vec<Group>,
}

fn build_groups(ed: &Editor, c: &Catalog, q: &str) -> Vec<Group> {
    let m = &ed.model;
    let rooms = ed
        .rooms
        .iter()
        .map(|r| {
            node(
                format!("{} {}", r.number, r.name).trim().to_string(),
                "",
                Act::None,
                vec![],
            )
        })
        .collect();
    let lists: [(&str, &'static str, Vec<Node>); 5] = [
        ("browser.group.datums", "grid", datums(ed, c)),
        ("browser.group.systems", "system", systems(m, c)),
        ("browser.group.families", "types", families(m, c)),
        ("browser.group.rooms", "room", rooms),
        ("browser.group.core", "room", core_elements(ed)),
    ];
    lists
        .into_iter()
        .enumerate()
        .map(|(salt, (title, icon, nodes))| {
            let nodes = if q.trim().is_empty() {
                nodes
            } else {
                filter(&nodes, q)
            };
            let mut rows = vec![];
            flatten(nodes, 0, salt as u64, &mut rows);
            Group {
                title: c.t(title),
                icon,
                rows,
                shown: vec![],
            }
        })
        .collect()
}

/// What to do after a click in the tree.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Do {
    Edit(Act),
    /// All instances of the type: in the project (true) or in the view.
    SelectAll(Act, bool),
    Replace(Act),
    Select(Act),
    Delete(Act),
}

/// Tree navigation keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

const NAV_KEYS: [(egui::Key, NavKey); 6] = [
    (egui::Key::ArrowUp, NavKey::Up),
    (egui::Key::ArrowDown, NavKey::Down),
    (egui::Key::ArrowLeft, NavKey::Left),
    (egui::Key::ArrowRight, NavKey::Right),
    (egui::Key::Home, NavKey::Home),
    (egui::Key::End, NavKey::End),
];

/// Keyboard navigation and scroll state of the tree.
#[derive(Default)]
struct Nav {
    /// Row with the cursor (node key).
    cursor: Option<u64>,
    /// Row to scroll into view once.
    scroll_to: Option<u64>,
    /// Selection that has already triggered a scroll.
    last_sel: Vec<Sel>,
}

/// Cursor step over the group's visible rows: `at` is the position in `shown`.
/// Returns the new position and the branch to toggle.
/// Left: an expanded branch collapses, otherwise move to the parent.
/// Right: a collapsed branch expands, an expanded one moves to its first child.
fn tree_nav(
    rows: &[Flat],
    shown: &[u32],
    open: &HashSet<u64>,
    force: bool,
    at: usize,
    key: NavKey,
) -> (usize, Option<u64>) {
    if shown.is_empty() {
        return (0, None);
    }
    let last = shown.len() - 1;
    let at = at.min(last);
    let f = &rows[shown[at] as usize];
    let branch = f.span > 1;
    let is_open = force || open.contains(&f.key);
    match key {
        NavKey::Up => (at.saturating_sub(1), None),
        NavKey::Down => ((at + 1).min(last), None),
        NavKey::Home => (0, None),
        NavKey::End => (last, None),
        NavKey::Left => {
            if branch && is_open && !force {
                return (at, Some(f.key));
            }
            let parent = (0..at)
                .rev()
                .find(|&j| rows[shown[j] as usize].depth < f.depth);
            (parent.unwrap_or(at), None)
        }
        NavKey::Right => {
            if !branch {
                (at, None)
            } else if !is_open {
                (at, Some(f.key))
            } else {
                ((at + 1).min(last), None)
            }
        }
    }
}

/// The branch chevron's centre and size come from the icon size tokens.
pub(crate) fn paint_chevron(p: &egui::Painter, m: egui::Pos2, open: bool, color: egui::Color32) {
    let a = ddc_ui::theme::icon_size::XS / 4.0;
    let pts = if open {
        vec![
            m + egui::vec2(-a, -a / 2.0),
            m + egui::vec2(0.0, a / 2.0),
            m + egui::vec2(a, -a / 2.0),
        ]
    } else {
        vec![
            m + egui::vec2(-a / 2.0, -a),
            m + egui::vec2(a / 2.0, 0.0),
            m + egui::vec2(-a / 2.0, a),
        ]
    };
    let w = ddc_ui::theme::stroke::HAIRLINE * 1.5;
    p.add(egui::Shape::line(pts, egui::Stroke::new(w, color)));
}

/// Icon centre in the row and the text start.
const LEAD_CX: f32 = ddc_ui::theme::space::XS + ddc_ui::theme::icon_size::SM / 2.0;
const TEXT_X: f32 =
    ddc_ui::theme::space::XS + ddc_ui::theme::icon_size::SM + ddc_ui::theme::space::SM;

/// Lead (chevron or icon) centre x and label x of a row: the same for branches and leaves.
pub(crate) fn slots(left: f32, depth: u16) -> (f32, f32) {
    let x = left + f32::from(depth) * INDENT;
    (x + LEAD_CX, x + TEXT_X)
}

/// A tree row is selected if its element is part of the selection.
fn is_selected(act: Act, sel: &[Sel]) -> bool {
    matches!(act, Act::Select(s, _) if sel.contains(&s))
}

/// Tree group. Height is computed for all rows, only those inside the scroll window are drawn,
/// so frame cost does not depend on their count. Returns the branch to toggle.
/// Keys are read only while a tree row has focus: an input field does not lose them.
#[allow(clippy::too_many_arguments)]
fn draw_rows(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    c: &Catalog,
    g: &Group,
    open: &HashSet<u64>,
    force: bool,
    sel: &[Sel],
    nav: &mut Nav,
    out: &mut Option<Do>,
) -> Option<u64> {
    let w = ui.available_width();
    let total = g.shown.len() as f32 * ROW_H;
    let (area, _) = ui.allocate_exact_size(egui::vec2(w, total), egui::Sense::hover());
    let clip = ui.clip_rect();
    let base = ui.id().with(("c.tree", g.icon));
    let row_rect = |k: usize| {
        egui::Rect::from_min_size(
            egui::pos2(area.left(), area.top() + k as f32 * ROW_H),
            egui::vec2(w, ROW_H),
        )
    };
    let mut toggle = None;
    let window = row_window(area.top(), (clip.top(), clip.bottom()), g.shown.len());
    let find = |key: u64| {
        g.shown
            .iter()
            .position(|&i| g.rows[i as usize].key == key)
    };
    if let Some(k) = nav.scroll_to.and_then(find) {
        // one row of room above: a pinned section header must not cover the row
        ui.scroll_to_rect(row_rect(k).expand2(egui::vec2(0.0, ROW_H)), None);
        nav.scroll_to = None;
    }
    if let Some(p) = nav.cursor.and_then(find) {
        let id = base.with(g.shown[p]);
        if ui.memory(|m| m.has_focus(id)) {
            // keep the arrows for the tree instead of the default focus hop
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let (key, enter, del) = ui.input_mut(|i| {
                let none = egui::Modifiers::NONE;
                let key = NAV_KEYS
                    .iter()
                    .find(|(k, _)| i.consume_key(none, *k))
                    .map(|x| x.1);
                (
                    key,
                    i.consume_key(none, egui::Key::Enter),
                    i.consume_key(none, egui::Key::Delete),
                )
            });
            let cur = &g.rows[g.shown[p] as usize];
            if let Some(key) = key {
                let (to, tg) = tree_nav(&g.rows, &g.shown, open, force, p, key);
                toggle = tg;
                if to != p {
                    let row = &g.rows[g.shown[to] as usize];
                    nav.cursor = Some(row.key);
                    // now: lane_c clears scroll_to at the end of the frame
                    ui.scroll_to_rect(row_rect(to).expand2(egui::vec2(0.0, ROW_H)), None);
                    ui.memory_mut(|m| m.request_focus(base.with(g.shown[to])));
                }
                ui.ctx().request_repaint();
            }
            if enter {
                if cur.span > 1 {
                    toggle = Some(cur.key);
                } else if matches!(cur.act, Act::Select(..)) {
                    *out = Some(Do::Select(cur.act));
                } else {
                    *out = Some(Do::Edit(cur.act));
                }
            }
            if del && matches!(cur.act, Act::Select(..)) {
                *out = Some(Do::Delete(cur.act));
            }
        }
    }
    // the focused row out of the window still interacts: egui drops the focus of an unseen
    // widget (stuck canvas keys) and a press elsewhere must surrender it
    if let Some(p) = nav.cursor.and_then(find).filter(|p| !window.contains(p)) {
        let _ = ui.interact(row_rect(p), base.with(g.shown[p]), egui::Sense::click());
    }
    for k in window {
        let at = g.shown[k];
        let f = &g.rows[at as usize];
        let r = row_rect(k);
        let resp = ui.interact(r, base.with(at), egui::Sense::click());
        let fg = ddc_ui::widgets::paint_state(ui, th, r, &resp, is_selected(f.act, sel));
        let branch = f.span > 1;
        // a type with no instances is greyed: it is a candidate for purge
        let typed = matches!(f.act, Act::Type(..) | Act::Comp(..));
        let unused = !branch && (f.act == Act::Empty || (typed && f.right.is_empty()));
        let tc = if unused { th.c("text.disabled") } else { fg };
        let x = r.left() + f32::from(f.depth) * INDENT;
        let (lead_x, text_x) = slots(r.left(), f.depth);
        if branch {
            let m = egui::pos2(lead_x, r.center().y);
            paint_chevron(ui.painter(), m, force || open.contains(&f.key), tc);
        } else {
            let px = ddc_ui::theme::icon_size::SM;
            let ic = egui::Rect::from_center_size(
                egui::pos2(lead_x, r.center().y),
                egui::Vec2::splat(px),
            );
            icons.paint(ui, &ddc_ui::widgets::icon_name(g.icon, false), px as u32, ic, tc);
        }
        let sp = ddc_ui::theme::space::SM;
        let mut room = r.right() - x - TEXT_X - sp;
        if !f.right.is_empty() {
            let gr = ui.painter().text(
                r.right_center() - egui::vec2(sp, 0.0),
                egui::Align2::RIGHT_CENTER,
                &f.right,
                th.font(Role::Numeric),
                th.c("text.secondary"),
            );
            room -= gr.width() + sp;
        }
        if resp.clicked() {
            nav.cursor = Some(f.key);
            resp.request_focus();
        }
        let font = th.font(if branch && f.depth == 0 {
            Role::BodyStrong
        } else {
            Role::Body
        });
        let text = ddc_ui::widgets::shell_kit::elided(ui, &f.label, font, tc, room);
        let cut = text.elided;
        let pos = egui::pos2(text_x, r.center().y - text.size().y / 2.0);
        ui.painter().galley(pos, text, tc);
        let resp = if cut {
            resp.on_hover_text(&f.label)
        } else {
            resp
        };
        if branch && resp.clicked() {
            toggle = Some(f.key);
        }
        if !branch && matches!(f.act, Act::Select(..)) && resp.clicked() {
            *out = Some(Do::Select(f.act));
        }
        if !branch && f.act != Act::Empty && resp.double_clicked() {
            *out = Some(Do::Edit(f.act));
        }
        menu(&resp, f.act, c, th, out);
    }
    toggle
}

fn menu(r: &egui::Response, act: Act, c: &Catalog, th: &Theme, out: &mut Option<Do>) {
    use crate::ctx_menu::{self, item_icon};
    if matches!(act, Act::None | Act::Empty) {
        return;
    }
    r.context_menu(|ui| match act {
        Act::Type(..) | Act::Comp(..) => {
            if item_icon(ui, th, Some("properties"), &c.t("browser.menu.edit_type"), ctx_menu::OPEN)
                .clicked()
            {
                *out = Some(Do::Edit(act));
                ui.close();
            }
            let pick = matches!(act, Act::Type(Fam::Wall | Fam::Slab, _));
            let sub = ddc_ui::widgets::menu_sub(
                ui,
                th,
                Some("select-all"),
                &c.t("browser.menu.select_all"),
                pick,
                |ui| {
                    for (proj, key) in [
                        (false, "browser.menu.in_view"),
                        (true, "browser.menu.in_project"),
                    ] {
                        if item_icon(ui, th, None, &c.t(key), "").clicked() {
                            *out = Some(Do::SelectAll(act, proj));
                            ui.close();
                        }
                    }
                },
            );
            if !pick {
                sub.on_hover_text(c.t("browser.menu.pick_in_plan"));
            }
            if item_icon(ui, th, Some("swap-type"), &c.t("browser.menu.replace"), "").clicked() {
                *out = Some(Do::Replace(act));
                ui.close();
            }
        }
        Act::System(_) => {
            if item_icon(ui, th, Some("properties"), &c.t("browser.menu.system_props"), ctx_menu::OPEN)
                .clicked()
            {
                *out = Some(Do::Edit(act));
                ui.close();
            }
        }
        Act::Select(..) => {
            if item_icon(ui, th, Some("select"), &crate::tr("core_tools.browser.select", &[]), ctx_menu::OPEN)
                .clicked()
            {
                *out = Some(Do::Select(act));
                ui.close();
            }
            ddc_ui::widgets::menu_divider(ui, th);
            if item_icon(ui, th, Some("delete"), &crate::tr("core_tools.browser.delete", &[]), ctx_menu::DELETE)
                .clicked()
            {
                *out = Some(Do::Delete(act));
                ui.close();
            }
        }
        Act::None | Act::Empty => {}
    });
}

/// Project browser groups (below views and levels).
pub fn lane_c(
    ui: &mut egui::Ui,
    th: &Theme,
    icons: &mut ddc_ui::icons::IconCache,
    c: &Catalog,
    ed: &mut Editor,
) -> bool {
    let (q, expand, mut open, tree, mut nav) = with(|s| {
        (
            s.query.clone(),
            s.expand,
            std::mem::take(&mut s.open),
            s.tree.take(),
            std::mem::take(&mut s.nav),
        )
    });
    let force = expand || !q.trim().is_empty();
    let key = (ed.revision, ed.level, ed.rooms.len(), c.lang);
    let mut tree = match tree {
        Some(t) if t.key == key && t.query == q => t,
        _ => Tree {
            key,
            groups: build_groups(ed, c, &q),
            query: q,
            force: !force,
        },
    };
    if tree.force != force {
        for g in &mut tree.groups {
            g.shown = visible(&g.rows, &open, force);
        }
        tree.force = force;
    }
    if nav.last_sel != ed.selection {
        nav.last_sel = ed.selection.clone();
        // bring the newly selected element into view once
        nav.scroll_to = tree
            .groups
            .iter()
            .flat_map(|g| g.shown.iter().map(move |&i| &g.rows[i as usize]))
            .find(|f| is_selected(f.act, &ed.selection))
            .map(|f| f.key);
    }
    let mut any = false;
    let mut out = None;
    let sel = ed.selection.clone();
    ui.vertical(|ui| {
        for g in &mut tree.groups {
            if g.rows.is_empty() {
                // the families of a new project: say so instead of hiding the section
                if g.icon == "types" && !force && super::browser_sec::section(ui, th, &g.title, &g.title, Some(0), None).open {
                    crate::ui_kit::empty_state_compact_lang(ui, c.lang == Lang::Ru, "families");
                }
                continue;
            }
            any = true;
            let n = g.rows.iter().filter(|f| f.depth == 0).count();
            if super::browser_sec::section(ui, th, &g.title, &g.title, Some(n), None).open {
                let toggled = draw_rows(ui, th, icons, c, g, &open, force, &sel, &mut nav, &mut out);
                if let Some(k) = toggled {
                    if !open.remove(&k) {
                        open.insert(k);
                    }
                    g.shown = visible(&g.rows, &open, force);
                }
            }
        }
    });
    let u = ed.tool.units;
    match out {
        Some(Do::Edit(Act::Type(Fam::Wall, id))) => {
            super::wall_type_editor::open(&ed.model, id, u);
        }
        Some(Do::Edit(Act::Type(f, id))) => {
            let mut d = TypeDraft::load(&ed.model, f, id, u);
            d.structure = f.layered();
            with(|s| type_dialog::open_in(&mut s.type_dlg, d, &ed.model));
        }
        Some(Do::Edit(Act::System(id))) => {
            let d = TypeDraft::load(&ed.model, Fam::Mep, id, u);
            with(|s| type_dialog::open_in(&mut s.type_dlg, d, &ed.model));
        }
        Some(Do::Edit(Act::Comp(fid, _))) => {
            if let Ok(f) = ed.model.family(fid) {
                with(|s| s.params = Some(Draft::from_family(&f)));
            }
        }
        Some(Do::SelectAll(act, project)) => {
            let level = (!project).then_some(ed.level);
            let insts = replace_type::instances(&ed.model, act, level);
            ed.selection = replace_type::as_selection(&insts);
            let what = replace_type::preview(&insts, c.lang == Lang::Ru);
            ed.status = Some(c.tf("browser.selected", &[("what", what.as_str())]));
        }
        Some(Do::Replace(act)) => replace_type::open(act),
        Some(Do::Select(act)) => select_row(ed, act),
        Some(Do::Delete(act)) => {
            // no level switch: an empty zone has no level
            if let Act::Select(s, _) = act {
                ed.selection = vec![s];
                ed.delete_selection();
            }
        }
        _ => {}
    }
    nav.scroll_to = None;
    with(|s| {
        s.open = open;
        s.tree = Some(tree);
        s.nav = nav;
    });
    any
}

/// Way out of the no-hit state: empties the search box.
pub fn clear_search() {
    with(|s| s.query.clear());
}

/// Widget id of the search field.
pub fn search_id() -> egui::Id {
    egui::Id::new("ddc-browser.search")
}

/// Search field above the tree, on the tree's row slots: the glyph where row icons sit, the text
/// where row labels start. Esc empties it and keeps focus; Ctrl+F focuses it from the browser.
pub fn search(ui: &mut egui::Ui, c: &Catalog) -> ddc_ui::widgets::field::FieldOutput {
    let th = ddc_ui::theme::of(ui.ctx());
    let id = search_id();
    // the context is read outside input_mut: its lock is not re-entrant
    let active = super::pane_focus::browser_active(ui.ctx());
    let wants = active
        && ui.input_mut(|i| {
            let hit = i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. }
                    if super::pane_focus::plain_ctrl_f(*key, *modifiers))
            });
            hit && i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)
        });
    if wants {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let hint = c.t("browser.search_hint");
    let tip = c.t("browser.reset_search");
    let w = ui.available_width();
    with(|s| {
        ddc_ui::widgets::field::TextField::new(&mut s.query)
            .id(id)
            .hint(&hint)
            .labelled_by(id.with("lbl"), &hint)
            .width(w)
            .search("find", LEAD_CX - ddc_ui::theme::icon_size::SM / 2.0, TEXT_X, &tip)
            .show(ui, &th)
    })
}

/// Acceptance-shot scenarios (`--shot-scenario C1..C4`, a CLI flag): they prepare the selection/windows
/// once. The older flag name is a hidden alias kept for existing screenshot scripts.
fn demo(app: &mut EditorApp) {
    let want = with(|s| {
        if s.demo.is_some() {
            return None;
        }
        let a: Vec<String> = std::env::args().collect();
        let v = a
            .iter()
            .position(|x| x == "--shot-scenario" || x == "--lane-c")
            .and_then(|i| a.get(i + 1).cloned())
            .unwrap_or_default();
        s.demo = Some(v.clone());
        (!v.is_empty()).then_some(v)
    });
    let Some(w) = want else { return };
    let ed = &mut app.ed;
    let u = ed.tool.units;
    match w.as_str() {
        "C1" => {
            let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
            if let Some(&last) = ids.last() {
                ed.edit_wall(last, None, None, Some(2.7));
            }
            ed.selection = ids.iter().map(|&i| Sel::Wall(i)).collect();
            if let Some(r) = ed.rooms.first() {
                ed.selection.push(Sel::Room(r.id));
            }
            ed.status = None;
        }
        "C2" => {
            let id = type_dialog::types_of(&ed.model, Fam::Wall)
                .into_iter()
                .find(|t| t.1.contains("380"))
                .map_or(0, |t| t.0);
            let mut d = TypeDraft::load(&ed.model, Fam::Wall, id, u);
            d.structure = true;
            with(|s| s.type_dlg = Some(d));
        }
        "C3" => {
            ed.selection.clear();
            if let Some(f) = ddc_model::library()
                .ok()
                .and_then(|v| v.into_iter().find(|f| f.name == "Table"))
            {
                let mut d = Draft::from_family(&f);
                if let Some(r) = d.rows.iter_mut().find(|r| r.name == "LegH") {
                    r.formula = "Height - Top + 2".into();
                }
                with(|s| s.params = Some(d));
            }
        }
        "C4" => {
            let m = &mut ed.model;
            for n in ["Table", "Chair", "WC"] {
                let _ = m.load_library_family(n);
            }
            let _ = m.add_member_type(
                "\u{41a}\u{43e}\u{43b}\u{43e}\u{43d}\u{43d}\u{430} 400×400",
                &ddc_model::MemberShape::Rect { b: 0.4, h: 0.4 },
                "Concrete",
            );
            let _ = m.add_catalog_member_type("HEA200");
            let lvl = ed.level;
            if let Ok(s) = m.add_mep_system("\u{41f}1", ddc_model::SystemKind::SupplyAir) {
                for (w, x) in [(0.4, 0.0), (0.4, 2.0), (0.25, 4.0)] {
                    let sec = ddc_model::MepSection::new(
                        ddc_model::MepDomain::Duct,
                        ddc_model::MepShape::Round { d: w },
                    );
                    let _ = m.add_mep_run(&ddc_model::RunSpec::new(
                        s,
                        lvl,
                        sec,
                        vec![(x, 1.0, 2.8), (x + 1.8, 1.0, 2.8)],
                    ));
                }
            }
            if let Ok(s) = m.add_mep_system("\u{412}1", ddc_model::SystemKind::ColdWater) {
                if let Ok(r) = ddc_model::RunSpec::pipe(
                    s,
                    lvl,
                    ddc_model::PipeMaterial::Steel,
                    25,
                    vec![(0.5, 3.0, 0.6), (3.5, 3.0, 0.6)],
                ) {
                    let _ = m.add_mep_run(&r);
                }
            }
            let _ = ed.reload();
            ed.status = None;
            with(|s| s.expand = true);
        }
        _ => {}
    }
}

/// Opens the type dialog on the first wall type, renamed to `name` (blank: a refused edit).
#[cfg(test)]
pub(crate) fn seed_type_draft_for_test(ed: &Editor, name: &str) {
    let mut d = TypeDraft::first(&ed.model, Fam::Wall, ed.tool.units);
    d.name = name.into();
    with(|s| s.type_dlg = Some(d));
}

/// The wall type names of the model, for checking a saved rename.
#[cfg(test)]
pub(crate) fn wall_type_names_for_test(m: &Model) -> Vec<String> {
    type_dialog::types_of(m, Fam::Wall).into_iter().map(|t| t.1).collect()
}

/// Why the type dialog refused, as it shows it.
#[cfg(test)]
pub(crate) fn type_error_for_test() -> Option<String> {
    with(|s| s.type_dlg.as_ref().and_then(|d| d.error.clone()))
}

/// Opens the parameter editor on the first project family (else the first library one) with
/// one added type; returns the family name.
#[cfg(test)]
pub(crate) fn seed_params_draft_for_test(ed: &Editor) -> String {
    let m = &ed.model;
    let f = m
        .families()
        .ok()
        .and_then(|v| v.first().and_then(|(id, _)| m.family(*id).ok()))
        .unwrap_or_else(|| ddc_model::library().unwrap().into_iter().next().unwrap());
    let mut d = Draft::from_family(&f);
    d.types.push(("Zz T".into(), vec![String::new(); d.rows.len()]));
    with(|s| s.params = Some(d));
    f.name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_question_sees_saves_and_discards_the_type_dialog() {
        let mut e = Editor::new_in_memory().unwrap();
        let c = Catalog::new(Lang::En);
        let stored = TypeDraft::first(&e.model, Fam::Wall, e.tool.units).stored_name().to_string();
        seed_type_draft_for_test(&e, &stored);
        assert_eq!(drafts(&e.model), (None, None), "an unedited dialog is no draft");
        seed_type_draft_for_test(&e, "Zz Type");
        assert_eq!(drafts(&e.model).0.as_deref(), Some(stored.as_str()));
        discard_drafts();
        assert!(with(|s| s.type_dlg.is_none()), "Discard closes it unsaved");
        assert!(!wall_type_names_for_test(&e.model).contains(&"Zz Type".to_string()));
        // a blank name refuses: the dialog stays with its reason, nothing is written
        seed_type_draft_for_test(&e, "  ");
        let rev = e.revision;
        assert!(!save_drafts(&mut e, &c));
        assert_eq!(e.revision, rev);
        assert!(type_error_for_test().is_some());
        assert!(drafts(&e.model).0.is_some(), "the draft is kept");
        with(|s| s.type_dlg.as_mut().unwrap().name = "Zz Type".into());
        assert!(save_drafts(&mut e, &c));
        assert!(wall_type_names_for_test(&e.model).contains(&"Zz Type".to_string()));
        assert_eq!(drafts(&e.model), (None, None));
    }

    #[test]
    fn swap_model_closes_the_type_dialog_and_parameter_editor() {
        let mut e = Editor::new_in_memory().unwrap();
        seed_type_draft_for_test(&e, "Zz Type");
        let _ = seed_params_draft_for_test(&e);
        assert!(drafts_at(&e).0.is_some());
        // a swap that fails keeps the old project, so its dialogs stay too
        let bad = Model::open(":memory:").unwrap();
        bad.sql().execute_batch("DROP TABLE wall_type_layer;").unwrap();
        assert!(e.replace_model(bad).is_err());
        assert!(with(|s| s.type_dlg.is_some() && s.params.is_some()));
        let fresh = Editor::new_in_memory().unwrap().model;
        e.replace_model(fresh).unwrap();
        assert!(with(|s| s.type_dlg.is_none() && s.params.is_none() && s.drafts_memo.is_none()));
        assert_eq!(drafts_at(&e), (None, None));
        assert_eq!(crate::project_ui::drafts::count(&e), 0, "no stale draft after the swap");
    }

    #[test]
    fn drafts_cache_follows_model_revision_and_draft_edits() {
        let mut e = Editor::new_in_memory().unwrap();
        let u = e.tool.units;
        let stored = TypeDraft::first(&e.model, Fam::Wall, u).stored_name().to_string();
        seed_type_draft_for_test(&e, &stored);
        let queries = || DRAFT_QUERIES.with(|q| q.get());
        assert_eq!(drafts_at(&e), (None, None));
        let n = queries();
        for _ in 0..5 {
            assert_eq!(drafts_at(&e), (None, None));
        }
        assert_eq!(queries(), n, "unchanged frames do not read the model");
        // an edit in the dialog is a new generation
        with(|s| s.type_dlg.as_mut().unwrap().name = "Zz Edited".into());
        assert_eq!(drafts_at(&e).0.as_deref(), Some(stored.as_str()));
        assert_eq!(queries(), n + 1);
        with(|s| s.type_dlg.as_mut().unwrap().name = stored.clone());
        assert_eq!(drafts_at(&e), (None, None));
        // a model change is a new revision: the stored type is renamed under the clean dialog
        let mut other = TypeDraft::first(&e.model, Fam::Wall, u);
        other.name = "Zz Renamed".into();
        let rev = e.revision;
        assert!(type_dialog::apply(&mut e, &mut other, false));
        assert_ne!(e.revision, rev);
        let q = queries();
        assert_eq!(drafts_at(&e).0.as_deref(), Some(stored.as_str()));
        assert_eq!(queries(), q + 1, "the new revision is read once");
        assert_eq!(crate::project_ui::drafts::count(&e), 1, "count asks the same cache");
        assert_eq!(queries(), q + 1);
        discard_drafts();
        assert_eq!(crate::project_ui::drafts::count(&e), 0);
    }

    /// Renames the stored wall type under a clean dialog: the dialog then holds a draft.
    fn rename_stored(e: &mut Editor) {
        let mut other = TypeDraft::first(&e.model, Fam::Wall, e.tool.units);
        other.name = "Zz Renamed".into();
        assert!(type_dialog::apply(e, &mut other, false));
    }

    #[test]
    fn drafts_cache_reads_anew_when_an_older_revision_is_put_back() {
        let mut e = Editor::new_in_memory().unwrap();
        let stored = TypeDraft::first(&e.model, Fam::Wall, e.tool.units).stored_name().to_string();
        seed_type_draft_for_test(&e, &stored);
        let rev = e.revision;
        assert_eq!(drafts_at(&e), (None, None));
        rename_stored(&mut e);
        // a level walk puts the saved revision back: the model is not what was cached under it
        e.restore_revision(rev);
        assert_eq!(e.revision, rev);
        assert_eq!(drafts_at(&e).0.as_deref(), Some(stored.as_str()), "changed draft -> dirty");
        assert_eq!(crate::project_ui::drafts::count(&e), 1);
        discard_drafts();
    }

    #[test]
    fn drafts_cache_follows_undo_back_to_an_older_revision() {
        let mut e = Editor::new_in_memory().unwrap();
        let stored = TypeDraft::first(&e.model, Fam::Wall, e.tool.units).stored_name().to_string();
        seed_type_draft_for_test(&e, &stored);
        let rev = e.revision;
        rename_stored(&mut e);
        assert_eq!(drafts_at(&e).0.as_deref(), Some(stored.as_str()));
        let renamed = e.revision;
        e.undo();
        assert_eq!(drafts_at(&e), (None, None), "undo brings the stored type back");
        // the walk restoring the rename's revision must not bring its answer back either
        e.restore_revision(renamed);
        assert_eq!(drafts_at(&e), (None, None));
        e.restore_revision(rev);
        assert_eq!(drafts_at(&e), (None, None));
        discard_drafts();
    }

    #[test]
    fn drafts_cache_after_swap_model_never_answers_for_the_old_model() {
        let mut e = Editor::new_in_memory().unwrap();
        let stored = TypeDraft::first(&e.model, Fam::Wall, e.tool.units).stored_name().to_string();
        let rev = e.revision;
        seed_type_draft_for_test(&e, "Zz Type");
        assert_eq!(drafts_at(&e).0.as_deref(), Some(stored.as_str()));
        e.replace_model(Editor::new_in_memory().unwrap().model).unwrap();
        e.restore_revision(rev);
        assert_eq!(drafts_at(&e), (None, None), "the swap closed the dialog");
        assert_eq!(crate::project_ui::drafts::count(&e), 0);
    }

    #[test]
    fn project_question_sees_saves_and_discards_the_parameter_editor() {
        let mut e = Editor::new_in_memory().unwrap();
        let c = Catalog::new(Lang::En);
        let f = ddc_model::library().unwrap().into_iter().next().unwrap();
        assert!(e.model.family_id(&f.name).unwrap().is_none());
        let edited = |f: &ddc_geom::component::Family| {
            let mut d = Draft::from_family(f);
            d.types.push(("Zz T".into(), vec![String::new(); d.rows.len()]));
            d
        };
        with(|s| s.params = Some(Draft::from_family(&f)));
        assert_eq!(drafts(&e.model), (None, None), "an unedited editor is no draft");
        // a library family is not in the project: OK refuses, so Save all does too
        with(|s| s.params = Some(edited(&f)));
        assert_eq!(drafts(&e.model).1.as_deref(), Some(f.name.as_str()));
        assert!(!save_drafts(&mut e, &c));
        assert!(with(|s| s.params.is_some()), "the editor stays open with its edits");
        discard_drafts();
        assert_eq!(drafts(&e.model), (None, None));
        assert!(load_family(&mut e, &f.name, &c));
        let fid = e.model.family_id(&f.name).unwrap().unwrap();
        let stored = e.model.family(fid).unwrap();
        with(|s| s.params = Some(edited(&stored)));
        assert!(drafts(&e.model).1.is_some());
        let n = e.model.comp_types(fid).unwrap().len();
        assert!(save_drafts(&mut e, &c));
        assert_eq!(e.model.comp_types(fid).unwrap().len(), n + 1);
        assert!(with(|s| s.params.is_none()), "OK closes the editor");
        assert_eq!(drafts(&e.model), (None, None));
    }

    #[test]
    fn load_and_save_params_are_single_undo_steps() {
        let mut e = crate::editor::Editor::new_in_memory().unwrap();
        let f = ddc_model::library().unwrap().into_iter().next().unwrap();
        let c = Catalog::new(Lang::Ru);
        let before = e.model.families().unwrap().len();
        assert!(load_family(&mut e, &f.name, &c));
        assert_eq!(e.model.families().unwrap().len(), before + 1);
        e.undo();
        assert_eq!(e.model.families().unwrap().len(), before);
        e.redo();
        let fid = e.model.family_id(&f.name).unwrap().unwrap();
        let mut d = Draft::from_family(&f);
        d.types
            .push(("New".into(), vec![String::new(); d.rows.len()]));
        let n = e.model.comp_types(fid).unwrap().len();
        assert!(save_params(&mut e, &d, &c));
        assert_eq!(e.model.comp_types(fid).unwrap().len(), n + 1);
        e.undo();
        assert_eq!(e.model.comp_types(fid).unwrap().len(), n);
        e.redo();
        assert_eq!(e.model.comp_types(fid).unwrap().len(), n + 1);
        let mut d2 = Draft::from_family(&f);
        d2.family = "no such family".into();
        assert!(!save_params(&mut e, &d2, &c));
    }

    fn leaf(l: &str) -> Node {
        node(l, "", Act::None, vec![])
    }

    #[test]
    fn a_huge_tree_draws_only_what_shows() {
        let nodes: Vec<Node> = (0..200_000).map(|i| leaf(&format!("R{i}"))).collect();
        let mut rows = vec![];
        flatten(nodes, 0, 0, &mut rows);
        let shown = visible(&rows, &HashSet::new(), false);
        assert_eq!(shown.len(), 200_000);
        let w = row_window(0.0, (240_000.0, 240_600.0), shown.len());
        assert_eq!(w, 10_000..10_025);
        assert_eq!(
            row_window(0.0, (-100.0, 48.0), 10),
            0..2,
            "the part above is cut"
        );
        assert_eq!(row_window(0.0, (500.0, 900.0), 10), 10..10, "scrolled past");
    }

    #[test]
    fn closed_branches_hide_their_subtree() {
        let t = vec![
            node(
                "a",
                "",
                Act::None,
                vec![node("b", "", Act::None, vec![leaf("c")]), leaf("d")],
            ),
            leaf("e"),
        ];
        let mut rows = vec![];
        flatten(t, 0, 0, &mut rows);
        assert_eq!(
            rows.iter().map(|r| r.span).collect::<Vec<_>>(),
            [4, 2, 1, 1, 1]
        );
        assert_eq!(visible(&rows, &HashSet::new(), false), [0, 4]);
        assert_eq!(visible(&rows, &HashSet::new(), true), [0, 1, 2, 3, 4]);
        let open: HashSet<u64> = [rows[0].key].into_iter().collect();
        assert_eq!(visible(&rows, &open, false), [0, 1, 3, 4]);
    }

    fn search_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    /// Left x of the painted line that reads `text`, nested shapes included.
    fn text_left(shapes: &[egui::epaint::ClippedShape], text: &str) -> Option<f32> {
        fn find(s: &egui::Shape, text: &str) -> Option<f32> {
            match s {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(t.pos.x + t.galley.rect.min.x)
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        shapes.iter().find_map(|c| find(&c.shape, text))
    }

    /// Right x of the painted line that reads `text`: (galley end, end as seen through its clip).
    fn text_right(shapes: &[egui::epaint::ClippedShape], text: &str) -> Option<(f32, f32)> {
        fn find(s: &egui::Shape, text: &str) -> Option<f32> {
            match s {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(t.pos.x + t.galley.rect.max.x)
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, text)),
                _ => None,
            }
        }
        shapes
            .iter()
            .find_map(|c| find(&c.shape, text).map(|r| (r, r.min(c.clip_rect.right()))))
    }

    /// One pass: the search field above one tree-row slot. Returns (field rect, row rect, left x
    /// of the shown text or hint, clear glyph rect, right x of that text as in `text_right`).
    /// The edit's widget rect is the whole frame (06b2512ad), so the text is read from what is
    /// painted.
    #[allow(clippy::type_complexity)]
    fn search_pass(
        ctx: &egui::Context,
        c: &Catalog,
        events: Vec<egui::Event>,
    ) -> (egui::Rect, egui::Rect, Option<f32>, Option<egui::Rect>, Option<(f32, f32)>) {
        let mut got = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(400.0, 300.0),
            )),
            events,
            ..Default::default()
        };
        let out = ctx.run(input, |cx| {
            egui::CentralPanel::default().show(cx, |ui| {
                let out = search(ui, c);
                let (row, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_H),
                    egui::Sense::hover(),
                );
                let clear = ui.ctx().read_response(search_id().with("clear")).map(|r| r.rect);
                got = Some((out.rect, row, clear));
            });
        });
        let (field, row, clear) = got.expect("drawn");
        let q = with(|s| s.query.clone());
        let shown = if q.is_empty() { c.t("browser.search_hint") } else { q };
        (field, row, text_left(&out.shapes, &shown), clear, text_right(&out.shapes, &shown))
    }

    fn esc() -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn search_field_sits_on_the_row_slots_in_both_languages() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            assert!(!c.t("browser.search_hint").is_empty());
            let ctx = search_ctx();
            let mut last = None;
            for _ in 0..3 {
                last = Some(search_pass(&ctx, &c, vec![]));
            }
            let (field, row, text, _, _) = last.unwrap();
            assert_eq!(field.left(), row.left(), "{lang:?}: left edge on the row edge");
            assert_eq!(field.right(), row.right(), "{lang:?}: right edge on the row edge");
            // the typed text starts where row labels start
            let x = text.expect("hint painted");
            assert!((x - (row.left() + TEXT_X)).abs() < 0.6, "{lang:?}: {x}");
            with(|s| s.query = "wall".into());
            for _ in 0..3 {
                last = Some(search_pass(&ctx, &c, vec![]));
            }
            let (_, row, text, _, _) = last.unwrap();
            let x = text.expect("text painted");
            assert!((x - (row.left() + TEXT_X)).abs() < 0.6, "{lang:?} typed: {x}");
            with(|s| s.query.clear());
        }
    }

    /// The clear glyph shows on hover or focus (06b2512ad); the test focuses the field.
    #[test]
    fn clear_glyph_shows_only_with_text() {
        let c = Catalog::new(Lang::En);
        let ctx = search_ctx();
        ctx.memory_mut(|m| m.request_focus(search_id()));
        let mut empty = None;
        for _ in 0..3 {
            empty = Some(search_pass(&ctx, &c, vec![]));
        }
        assert!(empty.unwrap().3.is_none(), "no glyph on an empty field");
        with(|s| s.query = "wall".into());
        let mut full = None;
        for _ in 0..3 {
            full = Some(search_pass(&ctx, &c, vec![]));
        }
        let (field, _, _, glyph, _) = full.unwrap();
        let g = glyph.expect("room for the clear glyph");
        let side = ddc_ui::theme::of(&ctx).d().control_sm + ddc_ui::theme::space::XS;
        let at_end = g.right() <= field.right() + 0.5 && g.left() >= field.right() - side - 0.5;
        assert!(at_end, "glyph {g:?} at the field's end {field:?}");
        // a query longer than the field: the text area gives up `side`, so what is painted of
        // the text ends left of the glyph (1 px: the edit's clip is grown by 1 for the cursor)
        with(|s| s.query = "wall ".repeat(40));
        let mut long = None;
        for _ in 0..3 {
            long = Some(search_pass(&ctx, &c, vec![]));
        }
        let (_, _, _, glyph, right) = long.unwrap();
        let g = glyph.expect("glyph with a long query");
        let (end, seen) = right.expect("long text painted");
        assert!(end > g.left(), "the query {end} reaches past the glyph {g:?}");
        assert!(seen <= g.left() + 1.5, "text seen up to {seen}, glyph from {}", g.left());
        with(|s| s.query.clear());
        let mut back = None;
        for _ in 0..3 {
            back = Some(search_pass(&ctx, &c, vec![]));
        }
        assert!(back.unwrap().3.is_none(), "glyph gone again");
    }

    #[test]
    fn escape_clears_the_search_and_keeps_focus() {
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let ctx = search_ctx();
            with(|s| s.query = "wall".into());
            ctx.memory_mut(|m| m.request_focus(search_id()));
            for _ in 0..3 {
                search_pass(&ctx, &c, vec![]);
            }
            assert!(ctx.memory(|m| m.has_focus(search_id())), "{lang:?}: focused");
            search_pass(&ctx, &c, vec![esc()]);
            assert!(with(|s| s.query.is_empty()), "{lang:?}: Esc clears");
            assert!(ctx.memory(|m| m.has_focus(search_id())), "{lang:?}: focus kept");
        }
    }

    #[test]
    fn search_has_no_raw_text_edit() {
        let src = include_str!("browser.rs");
        let from = src.find("pub fn search(").expect("search fn");
        let body = &src[from..src[from..].find("\n}\n").map_or(src.len(), |e| from + e)];
        assert!(!body.contains(concat!("egui::Text", "Edit")));
        assert!(body.contains("TextField::new"));
    }

    #[test]
    fn no_hit_way_out_clears_the_search() {
        with(|s| s.query = "zzz".into());
        clear_search();
        assert!(with(|s| s.query.is_empty()));
    }

    #[test]
    fn filter_keeps_matching_branches() {
        let t = vec![
            node(
                crate::app2_fx::fx("br_walls"),
                "",
                Act::None,
                vec![leaf("Masonry 200"), leaf("Exterior 380")],
            ),
            node(
                crate::app2_fx::fx("br_furniture"),
                "",
                Act::None,
                vec![node("Table", "", Act::None, vec![leaf("1600x800")])],
            ),
        ];
        let f = filter(&t, "1600");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kids[0].kids[0].label, "1600x800");
        let f = filter(&t, &crate::app2_fx::fx("br_walls_query"));
        assert_eq!(f[0].kids.len(), 2, "group match keeps its subtree");
        assert_eq!(filter(&t, "").len(), 2);
        assert!(filter(&t, "no such node").is_empty());
    }

    fn put_wall(ed: &mut Editor, wt: i64) {
        let level = ed.level;
        ed.model
            .add_wall(&ddc_model::WallSpec { a: (0.0, 0.0), b: (4.0, 0.0), level, base_offset: 0.0, height: 3.0, wall_type: wt, offset0: None })
            .unwrap();
    }

    /// A project with content: a wall, a loaded component family and script members.
    fn used_project() -> Editor {
        use ddc_model::{SolidForm, SolidFrame, SolidIfc, SolidSection, SolidSpec};
        let mut ed = Editor::new_in_memory().unwrap();
        let wt = type_dialog::types_of(&ed.model, Fam::Wall)[0].0;
        put_wall(&mut ed, wt);
        ed.model.load_library_family("Table").unwrap();
        let r = vec![ddc_geom::v2(0.0, 0.0), ddc_geom::v2(0.1, 0.0), ddc_geom::v2(0.1, 0.1), ddc_geom::v2(0.0, 0.1)];
        for name in ["chord", "chord", "post"] {
            ed.model
                .add_solid(&SolidSpec {
                    level: ed.level,
                    name: name.into(),
                    void: false,
                    material: "S355".into(),
                    ifc: Some(SolidIfc { class: "IfcMember".into(), predefined: "MEMBER".into() }),
                    frame: SolidFrame::default(),
                    form: SolidForm::Extrusion { profile: SolidSection::polygon(r.clone()), start: 0.0, end: 3.0 },
                })
                .unwrap();
        }
        ed
    }

    /// A new project lists no families (its seeded types are not in use); script members show
    /// as a family of their class with a row per part name.
    #[test]
    fn families_list_only_what_the_project_uses() {
        let c = Catalog::new(Lang::En);
        let bare = Editor::new_in_memory().unwrap();
        assert_eq!(families(&bare.model, &c), vec![]);
        let f = families(&used_project().model, &c);
        let mem = f.iter().find(|n| n.label == c.t("type_dialog.warn.cat.member")).expect("members family");
        let kids: Vec<(&str, &str)> = mem.kids.iter().map(|n| (n.label.as_str(), n.right.as_str())).collect();
        assert_eq!(kids, [("chord", "2"), ("post", "1")]);
    }

    /// Parts without an IFC class still show, as "Other solids".
    #[test]
    fn classless_parts_list_as_other_solids() {
        use ddc_model::{SolidForm, SolidFrame, SolidSection, SolidSpec};
        let mut ed = Editor::new_in_memory().unwrap();
        let r = vec![ddc_geom::v2(0.0, 0.0), ddc_geom::v2(0.1, 0.0), ddc_geom::v2(0.1, 0.1), ddc_geom::v2(0.0, 0.1)];
        ed.model
            .add_solid(&SolidSpec {
                level: ed.level,
                name: "rivet".into(),
                void: false,
                material: "steel".into(),
                ifc: None,
                frame: SolidFrame::default(),
                form: SolidForm::Extrusion { profile: SolidSection::polygon(r), start: 0.0, end: 1.0 },
            })
            .unwrap();
        let c = Catalog::new(Lang::En);
        let f = families(&ed.model, &c);
        let g = f.iter().find(|n| n.label == c.t("type_dialog.warn.cat.solid")).expect("other solids");
        assert_eq!(g.kids.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(), ["rivet"]);
    }

    #[test]
    fn families_and_systems_from_model() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        ed.model.load_library_family("Table").unwrap();
        let s = ed
            .model
            .add_mep_system("S1", ddc_model::SystemKind::SupplyAir)
            .unwrap();
        let lvl = ed.level;
        ed.model
            .add_mep_run(&ddc_model::RunSpec::new(
                s,
                lvl,
                ddc_model::MepSection::new(
                    ddc_model::MepDomain::Duct,
                    ddc_model::MepShape::Round { d: 0.4 },
                ),
                vec![(0.0, 0.0, 3.0), (2.0, 0.0, 3.0)],
            ))
            .unwrap();
        let c = Catalog::new(Lang::Ru);
        let f = families(&ed.model, &c);
        assert!(!f.iter().any(|n| n.label == crate::app2_fx::fx("br_walls")), "no wall is placed: no wall family");
        let wt = type_dialog::types_of(&ed.model, Fam::Wall)[0].0;
        put_wall(&mut ed, wt);
        let f = families(&ed.model, &c);
        assert_eq!(f[0].label, crate::app2_fx::fx("br_walls"));
        assert_eq!(f[0].kids.len(), 1, "only the type in use");
        assert_eq!(f[0].kids[0].right, "1");
        let furn = f.iter().find(|n| n.label == crate::app2_fx::fx("br_furniture")).unwrap();
        assert_eq!(furn.kids[0].label, "Table");
        assert_eq!(furn.kids[0].kids.len(), 4, "Table: three metric sizes and one imperial");
        let s = systems(&ed.model, &c);
        assert_eq!(s[0].label, crate::app2_fx::fx("br_ventilation"));
        assert_eq!(s[0].kids[0].label, "S1");
        assert_eq!(s[0].kids[0].kids[0].label, crate::app2_fx::fx("br_duct_d400"));
    }

    /// A category heading is the localised label, never the stored id (`air_terminal`, `door`).
    #[test]
    fn family_category_headings_are_never_raw_ids() {
        let mut ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut seen = std::collections::BTreeSet::new();
        for f in ddc_model::library().unwrap() {
            if seen.insert(f.category.name()) {
                ed.model.load_library_family(&f.name).unwrap();
            }
        }
        assert!(seen.len() >= 5, "the library spans several categories: {seen:?}");
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            // the system-family headings (walls, floors, ...) are what an empty project lists
            let bare = crate::editor::Editor::new_in_memory().unwrap();
            let sys: Vec<String> = families(&bare.model, &c).into_iter().map(|n| n.label).collect();
            for n in families(&ed.model, &c) {
                if sys.contains(&n.label) || n.kids.is_empty() {
                    continue;
                }
                let raw = n.label.contains('_') || n.label.chars().all(|ch| ch.is_ascii_lowercase());
                assert!(!raw, "{lang:?}: heading {:?} is a raw category id", n.label);
            }
        }
    }

    #[test]
    fn core_rows_select_on_the_right_level_and_delete() {
        use crate::editor::structx::sel;
        let mut e = Editor::new_in_memory().unwrap();
        let (a, b) = (e.level, e.model.add_level("B", 3.0).unwrap());
        let c = e.model.add_level("C", 6.0).unwrap();
        let spec = ddc_model::RampSpec::new(b, Some(c), vec![(0.0, 0.0), (40.0, 0.0)]);
        let r = e.model.add_ramp(&spec).unwrap();
        let s = e
            .model
            .add_room_sep(&ddc_model::SepSpec {
                level: a,
                a: (0.0, 0.0),
                b: (1.0, 0.0),
            })
            .unwrap();
        let room = e
            .model
            .add_room(&ddc_model::RoomSpec {
                level: c,
                name: "R".into(),
                number: "1".into(),
                point: (1.0, 1.0),
                height: 2.7,
            })
            .unwrap();
        let z = e
            .model
            .add_zone(&ddc_model::ZoneSpec {
                name: "Z".into(),
                rooms: vec![room],
            })
            .unwrap();
        e.reload().unwrap();
        // a ramp from another level is visible in the tree; a separator is a row, not a counter
        let rows = core_elements(&e);
        let group = |k: &str| {
            let label = crate::tr(k, &[]);
            rows.iter().find(|n| n.label == label).unwrap().clone()
        };
        let (ramps, zones, seps) = (
            group("selcat.ramp"),
            group("selcat.zone"),
            group("selcat.room_sep"),
        );
        assert_eq!(
            ramps.kids[0].act,
            Act::Select(sel("ramp", r).unwrap(), Some(b))
        );
        assert_eq!(
            zones.kids[0].act,
            Act::Select(sel("zone", z).unwrap(), Some(c))
        );
        assert_eq!(
            seps.kids[0].act,
            Act::Select(sel("room_sep", s).unwrap(), Some(a))
        );
        select_row(&mut e, ramps.kids[0].act);
        assert_eq!(e.level, b);
        assert_eq!(e.selection, vec![sel("ramp", r).unwrap()]);
        select_row(&mut e, zones.kids[0].act);
        assert_eq!(e.level, c);
        assert_eq!(e.selection, vec![sel("zone", z).unwrap()]);
        // a zone without rooms from legacy data: the row exists, no level, deletable
        e.model
            .sql()
            .execute("DELETE FROM zone_room WHERE zone=?1", [z])
            .unwrap();
        e.reload().unwrap();
        let act = core_act(&e, "zone", z);
        assert_eq!(act, Act::Select(sel("zone", z).unwrap(), None));
        if let Act::Select(sl, _) = act {
            e.selection = vec![sl];
            e.delete_selection();
        }
        assert!(e.model.zones().unwrap().is_empty());
    }

    #[test]
    fn shaft_row_selects_it_on_its_level_and_delete_is_one_step() {
        use crate::editor::structx::sel;
        let mut e = Editor::new_in_memory().unwrap();
        let (a, b) = (e.level, e.model.add_level("B", 3.0).unwrap());
        let id = e
            .model
            .add_shaft(&ddc_model::ShaftSpec {
                level: a,
                top_level: b,
                base_offset: 0.0,
                top_offset: 0.0,
                ring: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
            })
            .unwrap();
        e.set_level(b);
        e.reload().unwrap();
        let rows = core_elements(&e);
        let label = crate::tr("selcat.shaft", &[]);
        let group = rows.iter().find(|n| n.label == label).unwrap();
        assert_eq!(group.kids.len(), 1);
        assert_eq!(
            group.kids[0].act,
            Act::Select(sel("shaft", id).unwrap(), Some(a))
        );
        select_row(&mut e, group.kids[0].act);
        assert_eq!(e.level, a);
        assert_eq!(e.selection, vec![sel("shaft", id).unwrap()]);
        e.delete_selection();
        assert!(e.model.shaft_ids().unwrap().is_empty());
        e.undo();
        assert_eq!(e.model.shaft_ids().unwrap(), vec![id]);
    }

    #[test]
    fn datum_rows_select_their_element_and_go_to_a_level_that_shows_it() {
        use crate::editor::structx::sel;
        let mut e = crate::editor::Editor::new_in_memory().unwrap();
        let upper = e.model.add_level("U", 6.0).unwrap();
        let near = e
            .model
            .add_grid(&ddc_model::GridSpec::line("A", (0.0, 0.0), (0.0, 5.0)))
            .unwrap();
        let mut hidden = ddc_model::GridSpec::line("B", (4.0, 0.0), (4.0, 5.0));
        hidden.base_level = Some(upper);
        let far = e.model.add_grid(&hidden).unwrap();
        let plane = e.model.add_ref_plane("P", (0.0, 2.0), (5.0, 2.0)).unwrap();
        e.reload().unwrap();
        let lvl = e.level;
        let groups = datums(&e, &Catalog::new(Lang::En));
        assert_eq!(
            groups[0].kids[0].act,
            Act::Select(sel("grid", near).unwrap(), Some(lvl))
        );
        assert_eq!(
            groups[0].kids[1].act,
            Act::Select(sel("grid", far).unwrap(), Some(upper))
        );
        assert_eq!(
            groups[1].kids[0].act,
            Act::Select(sel("ref_plane", plane).unwrap(), Some(lvl))
        );
        select_row(&mut e, groups[0].kids[1].act);
        assert_eq!(e.level, upper);
        assert_eq!(e.selection, vec![sel("grid", far).unwrap()]);
    }

    /// A(A1, A2) B(B1(B11)) C
    fn fixture() -> Vec<Flat> {
        let n = |l: &str, a: Act, k: Vec<Node>| node(l, "", a, k);
        let nodes = vec![
            n(
                "A",
                Act::None,
                vec![n("A1", Act::None, vec![]), n("A2", Act::None, vec![])],
            ),
            n(
                "B",
                Act::None,
                vec![n("B1", Act::None, vec![n("B11", Act::None, vec![])])],
            ),
            n("C", Act::Select(Sel::Wall(7), None), vec![]),
        ];
        let mut rows = vec![];
        flatten(nodes, 0, 0, &mut rows);
        rows
    }

    fn key_of(rows: &[Flat], label: &str) -> u64 {
        rows.iter().find(|f| f.label == label).unwrap().key
    }

    fn nav(rows: &[Flat], open: &HashSet<u64>, at: usize, k: NavKey) -> (usize, Option<u64>) {
        let shown = visible(rows, open, false);
        tree_nav(rows, &shown, open, false, at, k)
    }

    #[test]
    fn tree_nav_up_down_clamp_and_skip_collapsed() {
        let rows = fixture();
        let none = HashSet::new();
        // collapsed: A, B, C
        assert_eq!(nav(&rows, &none, 0, NavKey::Down), (1, None));
        assert_eq!(nav(&rows, &none, 2, NavKey::Down), (2, None));
        assert_eq!(nav(&rows, &none, 0, NavKey::Up), (0, None));
        let open: HashSet<u64> = [key_of(&rows, "A")].into();
        // A, A1, A2, B, C
        assert_eq!(nav(&rows, &open, 2, NavKey::Down), (3, None));
        assert_eq!(nav(&rows, &open, 3, NavKey::Up), (2, None));
    }

    #[test]
    fn tree_nav_home_end() {
        let rows = fixture();
        let open: HashSet<u64> = [key_of(&rows, "A")].into();
        assert_eq!(nav(&rows, &open, 3, NavKey::Home), (0, None));
        assert_eq!(nav(&rows, &open, 1, NavKey::End), (4, None));
        let shown: Vec<u32> = vec![];
        assert_eq!(tree_nav(&rows, &shown, &open, false, 0, NavKey::End), (0, None));
    }

    #[test]
    fn tree_nav_left_collapses_then_jumps_to_parent() {
        let rows = fixture();
        let ka = key_of(&rows, "A");
        let open: HashSet<u64> = [ka].into();
        // Left on an open branch collapses it
        assert_eq!(nav(&rows, &open, 0, NavKey::Left), (0, Some(ka)));
        // Left on a child jumps to the parent
        assert_eq!(nav(&rows, &open, 2, NavKey::Left), (0, None));
        // Left on a closed top-level branch stays
        assert_eq!(nav(&rows, &HashSet::new(), 1, NavKey::Left), (1, None));
        // Left on a closed branch inside an open one goes to the parent
        let kb = key_of(&rows, "B");
        let deep: HashSet<u64> = [kb].into();
        // B, B1, C -> B1 is a closed branch
        assert_eq!(nav(&rows, &deep, 2, NavKey::Left), (1, None));
    }

    #[test]
    fn tree_nav_right_expands_then_enters() {
        let rows = fixture();
        let ka = key_of(&rows, "A");
        assert_eq!(nav(&rows, &HashSet::new(), 0, NavKey::Right), (0, Some(ka)));
        let open: HashSet<u64> = [ka].into();
        assert_eq!(nav(&rows, &open, 0, NavKey::Right), (1, None));
        // a leaf stays put
        assert_eq!(nav(&rows, &open, 1, NavKey::Right), (1, None));
    }

    #[test]
    fn tree_nav_forced_open_never_collapses() {
        let rows = fixture();
        let shown = visible(&rows, &HashSet::new(), true);
        assert_eq!(shown.len(), rows.len());
        // Left on a branch while a search forces all open moves to the parent or stays
        assert_eq!(
            tree_nav(&rows, &shown, &HashSet::new(), true, 0, NavKey::Left),
            (0, None)
        );
        assert_eq!(
            tree_nav(&rows, &shown, &HashSet::new(), true, 1, NavKey::Left),
            (0, None)
        );
        assert_eq!(
            tree_nav(&rows, &shown, &HashSet::new(), true, 0, NavKey::Right),
            (1, None)
        );
    }

    #[test]
    fn selected_rows_follow_the_editor_selection() {
        let rows = fixture();
        let c = rows.iter().find(|f| f.label == "C").unwrap();
        assert!(is_selected(c.act, &[Sel::Wall(7)]));
        assert!(!is_selected(c.act, &[Sel::Wall(8)]));
        assert!(!is_selected(c.act, &[]));
        assert!(!is_selected(Act::None, &[Sel::Wall(7)]));
    }

    #[test]
    fn every_family_group_is_a_branch_with_the_same_slots() {
        for lang in [Lang::En, Lang::Ru] {
            let ed = used_project();
            let c = Catalog::new(lang);
            let nodes = families(&ed.model, &c);
            assert!(nodes.len() >= 3);
            for n in &nodes {
                assert!(!n.kids.is_empty(), "{lang:?} {}: group without a child", n.label);
            }
            let mut rows = vec![];
            flatten(nodes, 0, 7, &mut rows);
            let groups: Vec<&Flat> = rows.iter().filter(|f| f.depth == 0).collect();
            assert!(groups.len() >= 3);
            let first = slots(10.0, groups[0].depth);
            for g in &groups {
                assert!(g.span > 1, "{lang:?} {}: leaf-shaped group", g.label);
                assert_eq!(slots(10.0, g.depth), first);
            }
            // a loaded family without types still shows the placeholder
            for p in rows.iter().filter(|f| f.act == Act::Empty) {
                assert_eq!(p.depth, 1);
                assert!(p.right.is_empty() && p.span == 1);
                assert_eq!(p.label, c.t("browser.fam.empty"));
            }
            // one lead and label slot for branch and leaf at the same depth
            assert_eq!(slots(0.0, 1), (slots(0.0, 0).0 + INDENT, slots(0.0, 0).1 + INDENT));
        }
    }

    #[test]
    fn family_group_expand_state_survives_a_rebuild() {
        let ed = used_project();
        let c = Catalog::new(Lang::En);
        let build = || {
            let mut rows = vec![];
            flatten(families(&ed.model, &c), 0, 2, &mut rows);
            rows
        };
        let a = build();
        let member = a.iter().position(|f| f.depth == 0 && f.span > 1).expect("a family with types");
        let key = a[member].key;
        let mut open = HashSet::new();
        assert!(!visible(&a, &open, false).contains(&(member as u32 + 1)));
        open.insert(key);
        let b = build();
        assert_eq!(b[member].key, key);
        assert!(visible(&b, &open, false).contains(&(member as u32 + 1)));
    }

    #[test]
    fn family_rows_share_one_draw_path() {
        let src = include_str!("browser.rs");
        let code = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        assert!(code.contains("fn or_empty"));
        assert!(code.contains("slots(r.left(), f.depth)"));
        assert!(code.contains("egui::pos2(text_x,"));
        assert!(code.contains("egui::pos2(lead_x,"));
    }

    #[test]
    fn type_menu_has_no_dead_rows() {
        let src = include_str!("browser.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for dead in ["browser.menu.later", "browser.menu.create", "browser.menu.purge"] {
            assert!(!code.contains(dead), "dead menu row {dead}");
        }
        assert!(!code.contains("add_enabled(false"));
    }
}
