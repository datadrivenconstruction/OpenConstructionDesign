// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Quick work (fewer clicks): a palette with arguments (e.g. wall 300 concrete), repeat of the last
//! command (Enter / right click), a recent-commands panel, custom keys with conflicts and import/export,
//! a per-category context menu, a selection-properties HUD showing "mixed" values, connected-wall selection,
//! selection sets, smart defaults, undo history with jumping, an "Undo" strip, and first-run tips.
//! Entry points are `register` (commands) and `frame` (once per frame from `EditorApp::ui`).
pub mod args;
pub mod chain;
pub mod defaults;
pub mod hist_win;
pub mod hud;
pub mod keys_win;
pub mod menu;
pub mod overlay;
pub mod sets;
pub mod sheet;
pub mod tipcard;

use crate::editor::Sel;
use crate::shell::EditorApp;
use ddc_ui::frecency;
use ddc_ui::keymap::{Bindings, UserKeys};
use ddc_ui::notify::Level;
use ddc_ui::tips::Tips;
use ddc_ui::uxstate::UxState;
use std::collections::{BTreeMap, HashSet};

/// Copied element parameters (Copy parameters -> Paste parameters).
#[derive(Clone, Debug, PartialEq)]
pub enum Clip {
    Wall {
        ty: i64,
        height: f64,
    },
    Opening {
        kind: crate::opening_tool::Kind,
        dims: [f64; 3],
    },
}

pub struct State {
    pub ux: UxState,
    pub keys: UserKeys,
    pub bind: Bindings,
    /// Bindings are built (`register`); otherwise the registry is used as is.
    pub built: bool,
    pub keys_open: bool,
    pub keys_ui: keys_win::Ui,
    pub sheet: sheet::Ui,
    pub hist_open: bool,
    /// AI batches waiting for the "revert all" confirmation.
    pub hist_ask: Option<Vec<i64>>,
    /// Undo step labels from commands: stack index -> (step-kind key, command label).
    pub labels: BTreeMap<usize, (String, String)>,
    /// Context menu: screen point; `menu_req` opens it at the view center (from the palette).
    pub menu: Option<egui::Pos2>,
    pub menu_req: bool,
    pub menu_fresh: bool,
    /// The menu is open showing the list of selection sets.
    pub menu_sets: bool,
    pub hud: hud::Ui,
    pub sets: sets::Sets,
    pub clip: Option<Clip>,
    /// Previous non-empty selection (the "Previous selection" command).
    pub prev_sel: Vec<Sel>,
    pub sel_seen: Vec<Sel>,
    /// The palette was open in the previous frame; the use counter and undo depth come from the same frame.
    pub pal_open: bool,
    pub seq_seen: u64,
    pub depth_seen: usize,
    /// Arguments of the last palette command (repeat applies them again).
    pub last_args: Option<(String, String)>,
    pub repeating: bool,
    /// Command that wrote a history entry while it ran, and that entry (`EditorApp::run`).
    pub ran: Option<(String, u64)>,
    pub tips: Tips,
    /// Active tool of the previous frame (smart defaults).
    pub tool_was: Option<String>,
    pub dirty: bool,
    pub saved_at: f64,
    /// Key of the open project (defaults and sets are per project); `None` means not examined yet.
    pub project: Option<String>,
    /// Undo step counter seen in the previous frame (history labels).
    pub pushes_seen: u64,
    /// Wall type created by palette arguments and not yet used.
    pub fresh_type: Option<args::Fresh>,
    /// Which (query, row) the palette hint was built for.
    pub hint_for: (String, String),
}

impl Default for State {
    fn default() -> Self {
        State {
            ux: UxState::default(),
            keys: UserKeys::default(),
            bind: Bindings::default(),
            built: false,
            keys_open: false,
            keys_ui: Default::default(),
            sheet: Default::default(),
            hist_open: false,
            hist_ask: None,
            labels: BTreeMap::new(),
            menu: None,
            menu_req: false,
            menu_fresh: false,
            menu_sets: false,
            hud: Default::default(),
            sets: Default::default(),
            clip: None,
            prev_sel: vec![],
            sel_seen: vec![],
            pal_open: false,
            seq_seen: 0,
            depth_seen: 0,
            last_args: None,
            repeating: false,
            ran: None,
            tips: Tips::start(true),
            tool_was: None,
            dirty: false,
            saved_at: 0.0,
            project: None,
            pushes_seen: 0,
            fresh_type: None,
            hint_for: Default::default(),
        }
    }
}

/// Settings folder; none in tests (user files are not touched).
pub fn dir() -> Option<std::path::PathBuf> {
    if cfg!(test) {
        None
    } else {
        crate::gfx::data_dir()
    }
}

/// Rebuild bindings after a key edit; two-letter ones go to the shell's `keys2`.
pub fn rebuild_keys(app: &mut EditorApp) {
    app.quick.bind = Bindings::build(&app.sh.reg, &app.quick.keys);
    app.keys2 = app.quick.bind.seqs.clone();
    app.quick.built = true;
}

/// Chord with Ctrl/Alt/an F-key -> command (custom keys override the registry). While a chord
/// is being recorded in the Keys window, commands do not launch.
pub fn chord(app: &EditorApp, key: egui::Key, m: egui::Modifiers) -> Option<String> {
    if app.quick.keys_ui.capture.is_some() {
        return None;
    }
    if !app.quick.built {
        return app.sh.reg.by_shortcut(key, m).map(|c| c.id.clone());
    }
    app.quick
        .bind
        .command(key, m)
        .filter(|id| !app.sh.reg.get(id).is_some_and(|c| c.hold))
        .map(String::from)
}

/// Command label in the UI language.
pub fn label(app: &EditorApp, id: &str) -> String {
    app.sh.cat.t(&format!("cmd.{id}"))
}

/// Command keys for the menu and panel: the first chord, otherwise two letters.
pub fn keys_text(app: &EditorApp, id: &str) -> String {
    let b = &app.quick.bind;
    if let Some(c) = b.chords_of(id).first() {
        return c.clone();
    }
    if !app.quick.built {
        if let Some(c) = app.sh.reg.get(id).and_then(|c| c.shortcuts.first()) {
            return c.clone();
        }
    }
    b.seqs
        .seq_of(id)
        .or_else(|| app.keys2.seq_of(id))
        .unwrap_or_default()
        .to_string()
}

/// Select mode with no active tool (editor, plan edit strip), input windows and
/// key recording: only then do Space/Enter/right click belong to the quick commands.
pub fn idle(app: &EditorApp) -> bool {
    !app.ed.is_busy()
        && !app.plan.b.active()
        && !app.sh.palette.open
        && app.recover.is_none()
        && app.blocking.is_none()
        && app.quick.keys_ui.capture.is_none()
}

fn ru(app: &EditorApp) -> bool {
    app.sh.cat.lang == ddc_ui::Lang::Ru
}

pub fn toast(app: &mut EditorApp, lvl: Level, key: &str, args: &[(&str, &str)]) {
    let t = app.sh.cat.tf(key, args);
    app.sh.toasts.push(lvl, t, None);
}

const COMMANDS: &[(&str, crate::shell::Handler)] = &[
    ("app.repeat", h_repeat),
    ("app.history", h_history),
    ("mgr.keymap", h_keymap),
    (keys_win::UNDO_CMD, keys_win::undo_import),
    (sheet::COMMAND, sheet::toggle),
    ("app.tips", h_tips),
    ("app.hud", h_hud),
    ("app.recent_bar", h_bar),
    ("app.menu", h_menu),
    ("sel.connected", h_connected),
    ("sel.run", h_run),
    ("sel.invert", h_invert),
    ("sel.prev", h_prev),
    ("sel.set_save", h_set_save),
    ("sel.sets", h_sets),
    ("edit.copy_params", h_copy_params),
    ("edit.paste_params", h_paste_params),
    ("set.theme", h_theme),
    ("set.lang", h_lang),
];

/// Commands of this module (`shell::REGISTER`): state loading, bindings, handlers.
pub fn register(app: &mut EditorApp) {
    let d = dir();
    let (ux, bad_ux) = UxState::load(d.as_deref());
    let (keys, bad_keys) = UserKeys::load_checked(d.as_deref());
    app.quick.ux = ux;
    app.quick.keys = keys;
    for bad in [bad_ux, bad_keys].into_iter().flatten() {
        let f = bad.display().to_string();
        toast(app, Level::Warning, "quick.file_bad", &[("file", &f)]);
    }
    let usage = std::mem::take(&mut app.quick.ux.usage);
    frecency::with(|u| {
        if u.cmds.is_empty() {
            *u = usage;
        }
    });
    app.quick.seq_seen = frecency::with(|u| u.seq);
    app.quick.tips = Tips::start(app.quick.ux.tips_done || cfg!(test));
    rebuild_keys(app);
    for (id, h) in COMMANDS {
        app.register_command(id, *h);
    }
    tipcard::register(app);
}

/// Once per frame after the views: command accounting, defaults, keys, menu, HUD, windows, strips.
pub fn frame(app: &mut EditorApp, ctx: &egui::Context, area: egui::Rect, fire: &mut Vec<String>) {
    let now = ctx.input(|i| i.time);
    track_project(app);
    track_labels(app);
    track_runs(app);
    if app.quick.fresh_type.is_some() && defaults::tool_key(&app.ed).as_deref() != Some("wall") {
        args::drop_fresh(app);
    }
    defaults::track(app);
    palette_hint(app);
    track_selection(app, ctx, area);
    sheet::keys(app, ctx, fire);
    keys(app, ctx, fire);
    menu::right_click(app, ctx, area);
    if app.quick.menu_req {
        app.quick.menu_req = false;
        app.quick.menu = Some(area.center() - egui::vec2(110.0, 120.0));
        app.quick.menu_fresh = true;
    }
    // `bar_off` is checked inside: hovering the status bar's button still shows the strip
    if !crate::ai::render_off::capturing() {
        overlay::recent_bar(app, ctx, area, fire);
    }
    hud::ui(app, ctx, area);
    menu::ui(app, ctx, fire);
    if app.quick.hist_open {
        hist_win::ui(app, ctx);
    }
    if app.quick.keys_open {
        keys_win::ui(app, ctx);
    }
    sheet::ui(app, ctx);
    overlay::tips(app, ctx, area);
    tipcard::frame(app, ctx, area);
    persist(app, now);
    app.quick.pal_open = app.sh.palette.open;
}

/// Project key: the model file (`""` means an in-memory model).
pub fn project_key(ed: &crate::editor::Editor) -> String {
    if let Some(p) = ed.project.path() {
        return p.display().to_string();
    }
    // the working copy changes every session: a project without a file has no key yet
    if ed.project.work_path().is_some() {
        return String::new();
    }
    ed.model
        .sql()
        .query_row("PRAGMA database_list", [], |r| r.get::<_, String>(2))
        .unwrap_or_default()
}

/// The project changed: load its defaults and selection sets.
fn track_project(app: &mut EditorApp) {
    let key = project_key(&app.ed);
    if app.quick.project.as_deref() == Some(key.as_str()) {
        return;
    }
    app.quick.ux.open_project(&key, frecency::now());
    let saved = app
        .quick
        .ux
        .here()
        .map(|p| p.sets.clone())
        .unwrap_or_default();
    app.quick.sets = sets::Sets::from_saved(&saved);
    app.quick.prev_sel.clear();
    // defaults of the new project; opening a file is not a tool start, so the view is not reset
    defaults::restore(&mut app.ed, &app.quick.ux);
    app.quick.tool_was = defaults::tool_key(&app.ed);
    app.quick.project = Some(key);
    app.quick.dirty = true;
}

/// Selection sets -> project state (saved with `ux.json`).
pub fn sync_sets(app: &mut EditorApp) {
    let v = app.quick.sets.to_saved();
    app.quick.ux.here_mut().sets = v;
    app.quick.dirty = true;
}

/// New undo steps replaced the undone ones: their labels are no longer valid.
fn track_labels(app: &mut EditorApp) {
    let p = app.ed.pushes;
    let fresh = p.saturating_sub(app.quick.pushes_seen);
    hist_win::prune(
        &mut app.quick.labels,
        app.ed.undo_len(),
        app.ed.redo_len(),
        fresh,
    );
    app.quick.pushes_seen = p;
}

/// Palette hint: how the arguments were read for the highlighted command (before launch).
fn palette_hint(app: &mut EditorApp) {
    let p = &app.sh.palette;
    if !p.open || p.query.trim().is_empty() {
        app.sh.palette.hint.clear();
        app.quick.hint_for = Default::default();
        return;
    }
    let items = p.items(&app.sh.reg, &app.sh.cat);
    let id = match items.get(p.sel) {
        Some(ddc_ui::widgets::palette::Item::Cmd(id)) => id.clone(),
        _ => String::new(),
    };
    let key = (p.query.clone(), id.clone());
    if app.quick.hint_for == key {
        return;
    }
    let hint = if id.is_empty() {
        String::new()
    } else {
        args::preview(app, &id, &key.0)
    };
    app.sh.palette.hint = hint;
    app.quick.hint_for = key;
}

/// A command ran (the use counter grew): palette arguments, history step label,
/// and the "Undo" strip for commands that changed the model.
pub(crate) fn track_runs(app: &mut EditorApp) {
    let (seq, last) = frecency::with(|u| (u.seq, u.order.first().cloned()));
    if seq != app.quick.seq_seen {
        app.quick.dirty = true;
        let mut id = last.unwrap_or_default();
        // "Repeat" from the palette: the query "repeat" (typed in Russian) is not an argument of the repeated command
        let was_repeat = id == "app.repeat";
        if was_repeat {
            id = frecency::with(|u| u.last_repeatable(&app.sh.reg).map(String::from))
                .unwrap_or_default();
        }
        let via_palette = app.quick.pal_open && !app.sh.palette.open;
        let q = app.sh.palette.query.clone();
        if via_palette && !was_repeat && !q.trim().is_empty() && frecency::recordable(&id) {
            let pushes = app.ed.pushes;
            args::apply(app, &id, &q);
            if let Some(e) = app.ed.entry_since(pushes) {
                app.quick.ran = Some((id.clone(), e));
            }
            app.quick.last_args = Some((id.clone(), q));
        } else if frecency::recordable(&id) && !app.quick.repeating {
            app.quick.last_args = None;
        }
        app.quick.repeating = false;
    }
    // Undo only for the entry the command itself wrote while it ran, still on top: opening a
    // menu or a window writes none, and an edit made in the same frame is not the command's
    if let Some((id, entry)) = app.quick.ran.take() {
        let depth = app.ed.undo_len();
        if app.ed.top_entry() == Some(entry) && frecency::recordable(&id) && depth > 0 {
            let kind = app.ed.history().0.last().cloned().unwrap_or_default();
            let name = label(app, &id);
            app.quick.labels.insert(depth - 1, (kind, name.clone()));
            let text = app.ed.status.take().unwrap_or(name);
            let cased = ddc_ui::notify::sentence_case(text.clone());
            app.sh.toasts.live.retain(|t| t.text != text && t.text != cased);
            let undo = app.undo_label();
            app.sh
                .toasts
                .push_step(Level::Info, text, undo, "edit.undo", Some(entry));
        }
    }
    // a relaunch inside arguments (level change) is not a new command
    app.quick.seq_seen = frecency::with(|u| u.seq).max(seq);
    app.quick.depth_seen = app.ed.undo_len();
}

/// Selection change: the previous selection is remembered, the HUD moves to the cursor.
fn track_selection(app: &mut EditorApp, ctx: &egui::Context, area: egui::Rect) {
    if app.ed.selection == app.quick.sel_seen {
        return;
    }
    let old = std::mem::replace(&mut app.quick.sel_seen, app.ed.selection.clone());
    if !old.is_empty() {
        app.quick.prev_sel = old;
    }
    let p = ctx.input(|i| i.pointer.latest_pos());
    app.quick.hud.moved(p, area);
}

/// Space opens the palette, Enter repeats the last command (only when idle and outside input fields).
fn keys(app: &mut EditorApp, ctx: &egui::Context, fire: &mut Vec<String>) {
    if ctx.wants_keyboard_input() || !idle(app) || app.quick.menu.is_some() {
        return;
    }
    let (space, enter) = ctx.input(|i| {
        let plain = i.modifiers.is_none();
        (
            plain && i.key_pressed(egui::Key::Space),
            plain && crate::ui_kit::fresh_key(i, egui::Key::Enter),
        )
    });
    let enter = enter && crate::ui_kit::canvas_owns_keys(ctx);
    if space {
        fire.push("app.palette".into());
    } else if enter {
        fire.push("app.repeat".into());
    }
}

/// Save state at most once per 5 s, if anything changed.
fn persist(app: &mut EditorApp, now: f64) {
    if !app.quick.dirty || now - app.quick.saved_at < 5.0 {
        return;
    }
    app.quick.saved_at = now;
    save_now(app);
}

/// Write `ux.json` now (atomically); also called on application exit.
pub fn save_now(app: &mut EditorApp) {
    app.quick.dirty = false;
    let Some(d) = dir() else { return };
    let mut ux = app.quick.ux.clone();
    ux.usage = frecency::with(|u| u.clone());
    ux.tips_done = app.quick.tips.done;
    if let Err(e) = ux.save(&d) {
        let e = e.to_string();
        toast(app, Level::Warning, "quick.save_err", &[("err", &e)]);
    }
}

fn h_repeat(app: &mut EditorApp) {
    let last = frecency::with(|u| u.last_repeatable(&app.sh.reg).map(String::from));
    let Some(id) = last else {
        toast(app, Level::Info, "quick.repeat.none", &[]);
        return;
    };
    let args = app.quick.last_args.clone();
    app.quick.repeating = true;
    app.run(&id);
    if let Some((aid, q)) = args {
        if aid == id {
            args::apply(app, &id, &q);
        }
    }
}

fn h_history(app: &mut EditorApp) {
    app.quick.hist_open = !app.quick.hist_open;
}

fn h_keymap(app: &mut EditorApp) {
    app.quick.keys_open = !app.quick.keys_open;
}

fn h_tips(app: &mut EditorApp) {
    app.quick.tips.reopen();
}

fn h_hud(app: &mut EditorApp) {
    app.quick.ux.hud_off = !app.quick.ux.hud_off;
    app.quick.hud.closed = false;
    app.quick.dirty = true;
}

fn h_bar(app: &mut EditorApp) {
    app.quick.ux.bar_off = !app.quick.ux.bar_off;
    app.quick.dirty = true;
}

fn h_menu(app: &mut EditorApp) {
    app.quick.menu_req = true;
}

fn h_sets(app: &mut EditorApp) {
    if app.quick.sets.list.is_empty() {
        toast(app, Level::Info, "quick.sets.none", &[]);
        return;
    }
    app.quick.menu_req = true;
    app.quick.menu_sets = true;
}

fn walls_in_selection(app: &EditorApp) -> Vec<i64> {
    app.ed
        .selection
        .iter()
        .filter_map(|s| match s {
            Sel::Wall(id) => Some(*id),
            _ => None,
        })
        .collect()
}

/// Connected walls (`run` means chain only) from each selected one; one grid for all queries,
/// and a wall already in the result does not start a new traversal.
fn select_walls(app: &mut EditorApp, run: bool) {
    let start = walls_in_selection(app);
    if start.is_empty() {
        toast(app, Level::Info, "quick.sel.need_wall", &[]);
        return;
    }
    let segs = chain::segs(&app.ed);
    let ix = chain::Index::new(&segs, chain::TOL);
    let mut seen: HashSet<i64> = HashSet::new();
    let mut out: Vec<Sel> = vec![];
    for id in start {
        if seen.contains(&id) {
            continue;
        }
        let got = if run { ix.run(id) } else { ix.connected(id) };
        for w in got {
            if seen.insert(w) {
                out.push(Sel::Wall(w));
            }
        }
    }
    let n = out.len().to_string();
    app.ed.selection = out;
    toast(app, Level::Info, "quick.sel.walls", &[("n", &n)]);
}

fn h_connected(app: &mut EditorApp) {
    select_walls(app, false);
}

fn h_run(app: &mut EditorApp) {
    select_walls(app, true);
}

fn h_invert(app: &mut EditorApp) {
    let ed = &app.ed;
    let v = &app.plan.b.vis;
    let cur = crate::plan_cull::SelIndex::new(&ed.selection);
    let inv: Vec<Sel> = crate::selection::all(ed)
        .into_iter()
        .filter(|s| v.visible(ed, *s) && !cur.contains(s))
        .collect();
    app.ed.selection = inv;
}

fn h_prev(app: &mut EditorApp) {
    let all = crate::selection::all(&app.ed);
    let prev: Vec<Sel> = app
        .quick
        .prev_sel
        .iter()
        .copied()
        .filter(|s| all.contains(s))
        .collect();
    if prev.is_empty() {
        toast(app, Level::Info, "quick.sel.no_prev", &[]);
        return;
    }
    app.ed.selection = prev;
}

fn h_set_save(app: &mut EditorApp) {
    if app.ed.selection.is_empty() {
        toast(app, Level::Info, "quick.sets.empty", &[]);
        return;
    }
    let base = app.sh.cat.t("quick.sets.name");
    let sel = app.ed.selection.clone();
    let i = app.quick.sets.save(&base, app.ed.level, &sel);
    sync_sets(app);
    let name = app.quick.sets.list[i].name.clone();
    let n = sel.len().to_string();
    toast(
        app,
        Level::Success,
        "quick.sets.saved",
        &[("name", &name), ("n", &n)],
    );
}

/// Parameters to copy: from the first wall or opening of the selection (a room or other kind
/// before it is passed over), with the element they come from.
pub fn clip_of(ed: &crate::editor::Editor) -> Option<(Clip, Sel)> {
    ed.selection.iter().find_map(|s| {
        let c = match s {
            Sel::Wall(id) => ed.walls.iter().find(|w| w.id == *id).map(|w| Clip::Wall {
                ty: w.wall_type,
                height: w.height,
            }),
            Sel::Opening(id) => ed.opening(*id).map(|o| Clip::Opening {
                kind: o.kind,
                dims: [o.width, o.height, o.sill],
            }),
            _ => None,
        };
        c.map(|c| (c, *s))
    })
}

fn h_copy_params(app: &mut EditorApp) {
    match clip_of(&app.ed) {
        Some((c, src)) => {
            app.quick.clip = Some(c);
            let (key, id) = match src {
                Sel::Wall(i) => ("lnp.clip_from_wall", i),
                Sel::Opening(i) => ("lnp.clip_from_opening", i),
                _ => ("quick.clip.copied", 0),
            };
            toast(app, Level::Info, key, &[("id", &id.to_string())]);
        }
        None => toast(app, Level::Info, "quick.clip.unsupported", &[]),
    }
}

/// Paste the copied parameters onto all selected elements of the same category: one operation.
pub fn paste_params(ed: &mut crate::editor::Editor, clip: &Clip) -> bool {
    use crate::shell::panels::{edit_openings, edit_walls};
    match clip {
        Clip::Wall { ty, height } => {
            let ids: Vec<i64> = ed
                .selection
                .iter()
                .filter_map(|s| match s {
                    Sel::Wall(i) => Some(*i),
                    _ => None,
                })
                .collect();
            !ids.is_empty() && edit_walls(ed, &ids, Some(*ty), Some(*height))
        }
        Clip::Opening { kind, dims } => {
            let ids: Vec<i64> = ed
                .selection
                .iter()
                .filter_map(|s| match s {
                    Sel::Opening(i) if ed.opening(*i).is_some_and(|o| o.kind == *kind) => Some(*i),
                    _ => None,
                })
                .collect();
            !ids.is_empty() && edit_openings(ed, &ids, dims.map(Some))
        }
    }
}

fn h_paste_params(app: &mut EditorApp) {
    let Some(c) = app.quick.clip.clone() else {
        toast(app, Level::Info, "quick.clip.none", &[]);
        return;
    };
    if !paste_params(&mut app.ed, &c) {
        let msg = app.ed.status.take();
        match msg {
            Some(m) => app.sh.toasts.push(Level::Warning, m, None),
            None => toast(app, Level::Info, "quick.clip.nothing", &[]),
        }
    }
}

fn h_theme(app: &mut EditorApp) {
    let dark = app.sh.theme.mode == ddc_ui::Mode::Dark;
    app.prefs.theme = if dark { "light" } else { "dark" }.into();
    app.prefs.save();
}

fn h_lang(app: &mut EditorApp) {
    let l = if ru(app) {
        ddc_ui::Lang::En
    } else {
        ddc_ui::Lang::Ru
    };
    app.sh.cat = ddc_ui::Catalog::new(l);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wall_tool::Mode;

    fn app() -> EditorApp {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        EditorApp::new(ed, None, "test".into())
    }

    fn rect(app: &mut EditorApp, x: f64) {
        use crate::wall_tool::Event;
        use ddc_geom::v2;
        app.ed.set_mode(Mode::Rect);
        app.ed.handle(Event::Press(v2(x, 0.0)));
        app.ed.handle(Event::Release(v2(x + 2.0, 2.0)));
        app.ed.set_mode(Mode::Select);
    }

    #[test]
    fn commands_registered_and_labelled() {
        let a = app();
        for (id, _) in COMMANDS {
            assert!(a.sh.reg.get(id).is_some(), "{id} not in registry");
            assert!(a.handlers.contains_key(*id), "{id}");
            for lang in [ddc_ui::Lang::Ru, ddc_ui::Lang::En] {
                let c = ddc_ui::Catalog::new(lang);
                assert!(c.table(lang).contains_key(&format!("cmd.{id}")), "{id}");
            }
        }
        assert!(a.quick.built);
        assert_eq!(a.keys2.command("WA"), Some("arch.wall"));
        assert_eq!(
            chord(&a, egui::Key::K, egui::Modifiers::COMMAND).as_deref(),
            Some("app.palette")
        );
    }

    #[test]
    fn repeat_last_and_selection_tools() {
        let mut a = app();
        rect(&mut a, 0.0);
        rect(&mut a, 5.0);
        // repeat takes the last repeatable command (not Undo, not itself)
        assert!(a.run("arch.wall"));
        a.ed.set_mode(Mode::Select);
        a.run("app.history");
        a.run("app.repeat");
        assert_eq!(a.ed.wall_mode(), Mode::Polyline);
        a.ed.set_mode(Mode::Select);
        a.run("app.repeat");
        assert_eq!(a.ed.wall_mode(), Mode::Polyline, "repeat after repeat");
        a.ed.set_mode(Mode::Select);
        // toggles and deletion are not repeated
        a.run("view.3d");
        a.view3d = false;
        a.run("app.repeat");
        assert!(!a.view3d);
        a.run("view.plan");
        // connected walls: from one wall, its rectangle
        let w0 = a.ed.walls[0].id;
        a.ed.selection = vec![Sel::Wall(w0)];
        a.run("sel.connected");
        assert_eq!(a.ed.selection.len(), 4);
        // invert selection gives the other 4 walls; previous selection brings back the first ones
        a.quick.sel_seen = a.ed.selection.clone();
        a.run("sel.invert");
        let walls = |a: &EditorApp| {
            a.ed.selection
                .iter()
                .filter(|s| matches!(s, Sel::Wall(_)))
                .count()
        };
        assert_eq!(walls(&a), 4);
        assert!(!a.ed.selection.contains(&Sel::Wall(w0)));
        let ctx = egui::Context::default();
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        track_selection(&mut a, &ctx, area);
        a.run("sel.prev");
        assert!(a.ed.selection.contains(&Sel::Wall(w0)));
        // selection sets
        a.run("sel.set_save");
        assert_eq!(a.quick.sets.list.len(), 1);
        a.ed.selection.clear();
        let got = a.quick.sets.recall(0, &a.ed);
        assert_eq!(got.len(), 4);
        // sets live in the project state and come back with it
        assert_eq!(a.quick.ux.here().map(|p| p.sets.len()), Some(1));
        let ux = a.quick.ux.clone();
        a.quick.sets = Default::default();
        a.quick.project = None;
        a.quick.ux = ux;
        track_project(&mut a);
        assert_eq!(a.quick.sets.list.len(), 1);
        assert_eq!(a.quick.sets.list[0].level, a.ed.level);
    }

    #[test]
    fn busy_editor_owns_space_and_enter() {
        let mut a = app();
        a.ed.set_mode(Mode::Select);
        assert!(idle(&a));
        a.ed.set_mode(Mode::Polyline);
        assert!(!idle(&a));
        a.ed.set_mode(Mode::Select);
        a.ed.grip.typing = Some((0, "1".into()));
        assert!(!idle(&a));
        a.ed.grip.typing = None;
        a.quick.keys_ui.capture = Some("app.history".into());
        assert!(!idle(&a));
        a.quick.keys_ui.capture = None;
        assert!(idle(&a));
    }

    #[test]
    fn repeat_via_palette_keeps_arguments() {
        let mut a = app();
        // palette command with arguments
        a.sh.palette.open = true;
        a.quick.pal_open = true;
        a.sh.palette.query = "\u{441}\u{442}\u{435}\u{43d}\u{430} 250".into(); // Russian "wall 250"
        a.sh.palette.open = false;
        a.run("arch.wall");
        track_runs(&mut a);
        assert_eq!(
            a.quick.last_args,
            Some(("arch.wall".into(), "\u{441}\u{442}\u{435}\u{43d}\u{430} 250".into()))
        );
        a.ed.set_mode(Mode::Select);
        // Russian "repeat" selected in the palette: the wall arguments are not lost
        a.quick.pal_open = true;
        a.sh.palette.query = "\u{43f}\u{43e}\u{432}\u{442}\u{43e}\u{440}".into();
        a.run("app.repeat");
        track_runs(&mut a);
        assert_eq!(
            a.quick.last_args,
            Some(("arch.wall".into(), "\u{441}\u{442}\u{435}\u{43d}\u{430} 250".into()))
        );
        // the palette hint shows the parse
        a.sh.palette.open = true;
        a.sh.palette.query = "\u{441}\u{442}\u{435}\u{43d}\u{430} 250".into();
        a.sh.palette.sel = 0;
        palette_hint(&mut a);
        assert!(a.sh.palette.hint.contains("250"), "{}", a.sh.palette.hint);
    }

    #[test]
    fn copy_paste_params_one_undo_step() {
        let mut a = app();
        rect(&mut a, 0.0);
        let ids: Vec<i64> = a.ed.walls.iter().map(|w| w.id).collect();
        let t0 = a.ed.types[0].id;
        a.ed.selection = vec![Sel::Wall(ids[0])];
        assert!(crate::shell::panels::edit_walls(
            &mut a.ed,
            &ids[..1],
            Some(t0),
            Some(2.4)
        ));
        a.run("edit.copy_params");
        assert_eq!(
            a.quick.clip,
            Some(Clip::Wall {
                ty: t0,
                height: 2.4
            })
        );
        a.ed.selection = ids.iter().map(|i| Sel::Wall(*i)).collect();
        let d0 = a.ed.undo_len();
        a.run("edit.paste_params");
        assert_eq!(a.ed.undo_len(), d0 + 1);
        assert!(a
            .ed
            .walls
            .iter()
            .all(|w| w.wall_type == t0 && (w.height - 2.4).abs() < 1e-9));
        a.run("edit.undo");
        assert_eq!(
            a.ed.walls
                .iter()
                .filter(|w| (w.height - 2.4).abs() < 1e-9)
                .count(),
            1
        );
    }

    #[test]
    fn copy_params_from_room_and_wall_uses_the_wall_and_names_it() {
        let mut a = app();
        rect(&mut a, 0.0);
        let w = a.ed.walls[1].id;
        a.ed.selection = vec![Sel::Room(1), Sel::Wall(w)];
        let (c, src) = clip_of(&a.ed).expect("the wall is the source");
        assert_eq!(src, Sel::Wall(w));
        assert!(matches!(c, Clip::Wall { .. }));
        a.run("edit.copy_params");
        assert_eq!(a.quick.clip, Some(c));
        // only kinds without parameters to copy: nothing is taken
        a.quick.clip = None;
        a.ed.selection = vec![Sel::Room(1)];
        assert!(clip_of(&a.ed).is_none());
        a.run("edit.copy_params");
        assert_eq!(a.quick.clip, None);
        for key in ["lnp.clip_from_wall", "lnp.clip_from_opening"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert!(en.contains("{id}") && ru.contains("{id}"), "{key}");
            assert_ne!(en, ru);
        }
    }
}
