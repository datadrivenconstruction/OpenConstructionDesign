// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View commands of the shell:
//! - `view.tabs`: the view tab strip stays on top even with only the plan and 3D open;
//! - `view.tile`: plan beside 3D (or beside the open section) in any layout;
//! - `view.sync_sel`: a selection made in one shown view frames itself in the other shown views
//!   (level of the plan, plan centre and scale, 3D camera);
//! - `view.goto`: a searchable list of every view (plans, 3D, sections, elevations, drafting
//!   views, sheets); Enter or a click opens one;
//! - `view.section_here`: a section through the current viewpoint (plan centre looking north,
//!   3D target looking along the camera, across an open section), one undo step, opened in a tab.
//!
//! Toggles live for the session. Strings come from the `view_cmds.*` catalog keys.

use std::cell::RefCell;

use crate::editor::sec::{display_name, MIN_DEPTH};
use crate::editor::{Editor, Sel};
use crate::shell::EditorApp;
use crate::ui_kit::{cancel_button, dialog_footer, empty_note, primary_click, Dialog, DialogSize};
use ddc_geom::{v2, V2};
use ddc_model::{SectionSpec, ViewKind};
use ddc_ui::i18n::Catalog;
use ddc_ui::theme::space;
use ddc_ui::widgets::type_pick::{fold, key_step};
use ddc_ui::widgets::TextField;

#[derive(Default)]
struct St {
    tabs: bool,
    tile: bool,
    sync: bool,
    /// Selection seen last frame (sync reacts to a change).
    last_sel: Vec<Sel>,
    /// Plan and 3D rectangles drawn last frame; `NOTHING` when not shown.
    plan: Option<egui::Rect>,
    d3: Option<egui::Rect>,
    goto: GotoUi,
}

#[derive(Default)]
struct GotoUi {
    open: bool,
    focus: bool,
    query: String,
    /// Highlighted row among the filtered ones.
    hi: usize,
}

thread_local! {
    static ST: RefCell<St> = RefCell::new(St::default());
}

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    ST.with(|s| f(&mut s.borrow_mut()))
}

/// The tab strip is shown even without open sections (`view.tabs`).
pub fn tabs_always() -> bool {
    with(|s| s.tabs)
}

/// Plan beside 3D or the open section (`view.tile`).
pub fn tiled() -> bool {
    with(|s| s.tile)
}

/// Selection sync between the shown views is on (`view.sync_sel`).
pub fn sync_on() -> bool {
    with(|s| s.sync)
}

/// Where the plan and the 3D view were drawn this frame (from `panel_views`).
pub fn note_rects(plan: egui::Rect, d3: egui::Rect) {
    with(|s| {
        s.plan = plan.is_positive().then_some(plan);
        s.d3 = d3.is_positive().then_some(d3);
    });
}

fn say(a: &mut EditorApp, key: &str) {
    a.ed.status = Some(crate::tr(key, &[]));
}

fn h_tabs(a: &mut EditorApp) {
    let on = with(|s| {
        s.tabs = !s.tabs;
        s.tabs
    });
    say(a, if on { "view_cmds.tabs.on" } else { "view_cmds.tabs.off" });
}

fn h_tile(a: &mut EditorApp) {
    let split_layout = matches!(
        a.workspace().views,
        ddc_ui::workspace::ViewArrangement::SplitPlan3d { .. }
    );
    if split_layout && !tiled() {
        return say(a, "view_cmds.tile.layout");
    }
    let on = with(|s| {
        s.tile = !s.tile;
        s.tile
    });
    // the plan pane changes size: frame the model now and again at the new pane size if cut
    a.plan.fit(&a.ed);
    a.plan.fit_if_cut = true;
    say(a, if on { "view_cmds.tile.on" } else { "view_cmds.tile.off" });
}

fn h_sync(a: &mut EditorApp) {
    let sel = a.ed.selection.clone();
    let on = with(|s| {
        s.sync = !s.sync;
        // the selection already made is not a new one
        s.last_sel = sel;
        s.sync
    });
    say(a, if on { "view_cmds.sync.on" } else { "view_cmds.sync.off" });
}

fn h_goto(_: &mut EditorApp) {
    with(|s| {
        s.goto = GotoUi {
            open: true,
            focus: true,
            ..GotoUi::default()
        };
    });
}

fn h_section_here(a: &mut EditorApp) {
    let Some((at, dir)) = viewpoint(a) else {
        return say(a, "view_cmds.here.no_view");
    };
    let bb = a.ed.model.plan_bbox().ok().flatten();
    let name = a.ed.next_section_name();
    let Some(s) = section_through(bb, at, dir, &name) else {
        return say(a, "view_cmds.here.no_view");
    };
    if let Some(id) = a.ed.add_view(&s) {
        a.open_view(id);
    }
}

/// `view.zoom_sel`: the plan frames what the selection draws there (rooms and slabs included),
/// 3D frames the selected solids.
fn h_zoom_sel(a: &mut EditorApp) {
    if a.ed.selection.is_empty() && a.ed.highlight().is_none() {
        return say(a, "v3d.msg.no_sel");
    }
    if a.view3d {
        if selection_box(a).is_some() {
            a.run("view.zoom_sel_3d");
        } else {
            say(a, "cmd_buttons.zoom_sel.no_shape");
        }
    } else if !crate::open::summary::frame_plan(a) {
        say(a, "cmd_buttons.zoom_sel.no_shape");
    }
}

/// One entry of the view tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewTab {
    Plan,
    D3,
    Section(i64),
    Drafting(i64),
    Sheet(i64),
}

/// The tabs in strip order and the one shown now.
pub(crate) fn view_tabs(a: &EditorApp) -> (Vec<ViewTab>, ViewTab) {
    let mut v = vec![ViewTab::Plan, ViewTab::D3];
    v.extend(
        a.tabs
            .iter()
            .copied()
            .filter(|id| a.ed.view_spec(*id).is_some())
            .map(ViewTab::Section),
    );
    let sheet = crate::sheet_view::active();
    let drafting = crate::drafting_view::current(&a.ed).filter(|_| sheet.is_none());
    if let Some(d) = drafting {
        v.push(ViewTab::Drafting(d));
    }
    if let Some(s) = sheet.filter(|s| a.ed.model.sheet(*s).is_ok()) {
        v.push(ViewTab::Sheet(s));
    }
    let cur = match (sheet, drafting) {
        (Some(s), _) => ViewTab::Sheet(s),
        (None, Some(d)) => ViewTab::Drafting(d),
        _ if a.view3d => ViewTab::D3,
        _ => a.section.map_or(ViewTab::Plan, ViewTab::Section),
    };
    (v, cur)
}

fn show_tab(a: &mut EditorApp, t: ViewTab) {
    match t {
        ViewTab::Plan => {
            crate::sheet_view::deactivate();
            crate::drafting_view::deactivate();
            a.view3d = false;
            a.section = None;
        }
        ViewTab::D3 => {
            crate::sheet_view::deactivate();
            crate::drafting_view::deactivate();
            a.view3d = true;
        }
        ViewTab::Section(id) => a.open_view(id),
        // a drafting view or a sheet is only listed while it is the open one
        ViewTab::Drafting(_) | ViewTab::Sheet(_) => {}
    }
}

/// `view.next`: the tab after the shown one, the last wraps to the plan.
fn h_next(a: &mut EditorApp) {
    let (tabs, cur) = view_tabs(a);
    let i = tabs.iter().position(|t| *t == cur).unwrap_or(0);
    show_tab(a, tabs[(i + 1) % tabs.len()]);
}

/// `view.close`: closes the shown section, elevation, drafting view or sheet; the plan and
/// 3D tabs stay.
fn h_close(a: &mut EditorApp) {
    match view_tabs(a).1 {
        ViewTab::Section(id) => a.close_view(id),
        ViewTab::Drafting(_) => crate::drafting_view::deactivate(),
        ViewTab::Sheet(_) => crate::sheet_view::deactivate(),
        ViewTab::Plan | ViewTab::D3 => say(a, "cmd_buttons.close.base"),
    }
}

/// Registration in the shell (`shell::REGISTER`).
pub fn register(app: &mut EditorApp) {
    app.register_command("view.zoom_sel", h_zoom_sel);
    app.register_command("view.next", h_next);
    app.register_command("view.close", h_close);
    app.register_command("view.tabs", h_tabs);
    app.register_command("view.tile", h_tile);
    app.register_command("view.sync_sel", h_sync);
    app.register_command("view.goto", h_goto);
    app.register_command("view.section_here", h_section_here);
}

// ---------- section here ----------

/// Point and horizontal look direction (unit) of the shown view; `None` on a sheet or a
/// drafting view, which have no model viewpoint.
fn viewpoint(a: &EditorApp) -> Option<(V2, V2)> {
    if crate::sheet_view::active().is_some() || crate::drafting_view::current(&a.ed).is_some() {
        return None;
    }
    if a.view3d {
        let c = &a.view.camera;
        let (sy, cy) = c.yaw.sin_cos();
        // the camera looks against `back()`; its yaw gives the horizontal part even top-down
        let dir = v2(-f64::from(cy), -f64::from(sy));
        return Some((v2(f64::from(c.target.x), f64::from(c.target.y)), dir));
    }
    if let Some(s) = a.section.and_then(|id| a.ed.view_spec(id)) {
        // across the open section: through the middle of its depth, looking along its line
        let (p, q) = crate::editor::sec::ends(s);
        let d = q - p;
        let l = d.len();
        if l < 1e-9 {
            return None;
        }
        let n = crate::editor::sec::look(s);
        let half = if s.kind == ViewKind::Section { s.depth / 2.0 } else { 0.0 };
        return Some(((p + q) * 0.5 + n * half, d * (1.0 / l)));
    }
    Some((a.plan.center, v2(0.0, 1.0)))
}

/// Margin around the model on both ends of the line and beyond the far side, m.
const HERE_MARGIN: f64 = 1.0;
/// Half length of the line when the model is empty, m.
const HERE_HALF: f64 = 5.0;

/// Section through `at` looking along `dir` (unit, horizontal): the line spans the plan box `bb`
/// across the look with a margin, the depth reaches its far side. Without a box (empty model) or
/// with the model behind, a 10 m line with the default depth.
pub(crate) fn section_through(
    bb: Option<[f64; 4]>,
    at: V2,
    dir: V2,
    name: &str,
) -> Option<SectionSpec> {
    let l = dir.len();
    if !(l > 1e-9) || !at.x.is_finite() || !at.y.is_finite() {
        return None;
    }
    let d = dir * (1.0 / l);
    // the look is to the left of a->b: u = (d.y, -d.x) gives left(u) = d
    let u = v2(d.y, -d.x);
    let corners = bb.map(|b| [v2(b[0], b[1]), v2(b[2], b[1]), v2(b[2], b[3]), v2(b[0], b[3])]);
    let dot = |p: V2, w: V2| (p - at).x * w.x + (p - at).y * w.y;
    let (t0, t1, depth) = match corners {
        Some(cs) => {
            let ts: Vec<f64> = cs.iter().map(|c| dot(*c, u)).collect();
            let far = cs.iter().map(|c| dot(*c, d)).fold(f64::MIN, f64::max);
            let t0 = ts.iter().copied().fold(f64::MAX, f64::min).min(-HERE_HALF / 5.0);
            let t1 = ts.iter().copied().fold(f64::MIN, f64::max).max(HERE_HALF / 5.0);
            let depth = if far > 0.0 {
                far + HERE_MARGIN
            } else {
                crate::editor::sec::DEFAULT_DEPTH
            };
            (t0 - HERE_MARGIN, t1 + HERE_MARGIN, depth)
        }
        None => (-HERE_HALF, HERE_HALF, crate::editor::sec::DEFAULT_DEPTH),
    };
    let (a, b) = (at + u * t0, at + u * t1);
    Some(SectionSpec {
        kind: ViewKind::Section,
        name: name.into(),
        a: (a.x, a.y),
        b: (b.x, b.y),
        depth: depth.max(MIN_DEPTH),
        far_clip: true,
    })
}

// ---------- selection sync ----------

/// Bounding box of the selection in the 3D scene (world metres), every selected range.
pub(crate) fn selection_box(a: &EditorApp) -> Option<ddc_render::Aabb> {
    a.ed
        .highlight_all()
        .into_iter()
        .filter_map(|r| a.scene.bounds_of(r))
        .reduce(|x, y| ddc_render::Aabb {
            min: x.min.min(y.min),
            max: x.max.max(y.max),
        })
}

/// The one level all selected elements are on, when it is not the shown one.
fn other_level(ed: &Editor) -> Option<i64> {
    let rows = crate::query::rows(ed);
    let mut lv = rows
        .iter()
        .filter(|r| ed.selection.contains(&r.sel))
        .map(|r| r.level);
    let first = lv.next()?;
    (lv.all(|l| l == first) && first != ed.level).then_some(first)
}

/// Frames a new selection in the shown views it was not made in: `pointer` over a view marks
/// it as the origin; from elsewhere (table, browser, palette) every shown view follows.
fn sync(a: &mut EditorApp, pointer: Option<egui::Pos2>) {
    let sel = a.ed.selection.clone();
    let (on, changed, plan, d3) = with(|s| {
        let changed = s.last_sel != sel;
        if changed {
            s.last_sel = sel.clone();
        }
        (s.sync, changed, s.plan, s.d3)
    });
    if !on || !changed || sel.is_empty() {
        return;
    }
    let inside = |r: Option<egui::Rect>| r.zip(pointer).is_some_and(|(r, p)| r.contains(p));
    if plan.is_some() && !inside(plan) {
        if let Some(l) = other_level(&a.ed) {
            a.ed.set_level(l);
            a.ed.selection = sel.clone();
        }
        match selection_box(a) {
            Some(b) => {
                let lo = v2(f64::from(b.min.x), f64::from(b.min.y));
                let hi = v2(f64::from(b.max.x), f64::from(b.max.y));
                a.plan.center = (lo + hi) * 0.5;
                a.plan.scale = crate::open::first_view::scale_for(lo, hi);
            }
            None => {
                crate::open::summary::frame_plan(a);
            }
        }
    }
    if d3.is_some() && !inside(d3) {
        if let Some(b) = selection_box(a) {
            let aspect = a.view.nav.aspect;
            a.view.nav.walk = None;
            a.view.nav.fit_animated(&mut a.view.camera, Some(b), aspect);
        }
    }
    with(|s| s.last_sel = a.ed.selection.clone());
}

// ---------- go to view ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dest {
    Plan(i64),
    D3,
    View(i64),
    Drafting(i64),
    Sheet(i64),
}

/// One row of the list: where it goes, its name and the kind word.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    pub dest: Dest,
    pub name: String,
    pub kind: String,
}

/// Every view of the project in browser order.
pub(crate) fn items(ed: &Editor, c: &Catalog) -> Vec<Item> {
    let mut v = vec![];
    let plan = c.t("browser.plan");
    for (id, name, _) in ed.levels() {
        v.push(Item {
            dest: Dest::Plan(id),
            name: format!("{plan} \u{b7} {name}"),
            kind: c.t("view_cmds.goto.kind.plan"),
        });
    }
    v.push(Item {
        dest: Dest::D3,
        name: c.t("cmd.view.3d"),
        kind: c.t("view_cmds.goto.kind.3d"),
    });
    for (id, s) in &ed.views {
        let kind = match s.kind {
            ViewKind::Section => "view_cmds.goto.kind.section",
            _ => "view_cmds.goto.kind.elevation",
        };
        v.push(Item {
            dest: Dest::View(*id),
            name: display_name(s),
            kind: c.t(kind),
        });
    }
    for d in ed.model.draftings().unwrap_or_default() {
        v.push(Item {
            dest: Dest::Drafting(d.id),
            name: d.name,
            kind: c.t("view_cmds.goto.kind.drafting"),
        });
    }
    for (id, s) in ed.model.sheets().unwrap_or_default() {
        v.push(Item {
            dest: Dest::Sheet(id),
            name: format!("{} {}", s.number, s.name),
            kind: c.t("view_cmds.goto.kind.sheet"),
        });
    }
    v
}

/// Rows whose name or kind matches the query (case and spaces ignored).
pub(crate) fn filtered(items: &[Item], query: &str) -> Vec<usize> {
    let q = fold(query);
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| {
            q.is_empty() || fold(&it.name).contains(&q) || fold(&it.kind).contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

/// The view shown now, for marking its row.
fn current(a: &EditorApp) -> Dest {
    if let Some(s) = crate::sheet_view::active() {
        return Dest::Sheet(s);
    }
    if let Some(d) = crate::drafting_view::current(&a.ed) {
        return Dest::Drafting(d);
    }
    if a.view3d {
        return Dest::D3;
    }
    a.section.map_or(Dest::Plan(a.ed.level), Dest::View)
}

/// Shows `d` in the view area, as the browser and the tabs do.
pub(crate) fn go(a: &mut EditorApp, d: Dest) {
    match d {
        Dest::Plan(l) => {
            crate::sheet_view::deactivate();
            crate::drafting_view::deactivate();
            a.ed.set_level(l);
            a.view3d = false;
            a.section = None;
        }
        Dest::D3 => {
            crate::sheet_view::deactivate();
            crate::drafting_view::deactivate();
            a.view3d = true;
            a.section = None;
        }
        Dest::View(id) => a.open_view(id),
        Dest::Drafting(id) => crate::drafting_view::open(id),
        Dest::Sheet(id) => crate::sheet_view::open(id),
    }
}

/// Search glyph and text offsets of the filter field, as in the find window.
const SEARCH_ICON_X: f32 = space::XS;
const SEARCH_TEXT_X: f32 = space::XS + ddc_ui::theme::icon_size::SM + space::SM;

fn goto_body(
    ui: &mut egui::Ui,
    c: &Catalog,
    st: &mut GotoUi,
    items: &[Item],
    cur: Dest,
    pick: &mut Option<Dest>,
    close: &mut bool,
) {
    let th = ddc_ui::theme::of(ui.ctx());
    let (hint, clear) = (c.t("view_cmds.goto.search"), c.t("find_replace.find_clear"));
    let out = TextField::new(&mut st.query)
        .id_salt("cmd1.goto.query")
        .labelled_by(egui::Id::new("cmd1.goto.query").with("lbl"), &hint)
        .hint(&hint)
        .search("find", SEARCH_ICON_X, SEARCH_TEXT_X, &clear)
        .width(ui.available_width())
        .show(ui, &th);
    if std::mem::take(&mut st.focus) {
        out.response.request_focus();
    }
    if out.changed {
        st.hi = 0;
    }
    let shown = filtered(items, &st.query);
    let (up, down, enter) = ui.input_mut(|i| {
        let none = egui::Modifiers::NONE;
        (
            i.consume_key(none, egui::Key::ArrowUp),
            i.consume_key(none, egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
        )
    });
    if up {
        st.hi = key_step(st.hi, shown.len(), -1);
    }
    if down {
        st.hi = key_step(st.hi, shown.len(), 1);
    }
    st.hi = st.hi.min(shown.len().saturating_sub(1));
    ui.add_space(space::XS);
    if shown.is_empty() {
        empty_note(ui, c.t("view_cmds.goto.none"), Some(c.t("view_cmds.goto.none_hint").as_str()));
    } else {
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * space::XS;
        let height = (ui.available_height() - space::XXXL).max(row_h * 3.0);
        egui::ScrollArea::vertical()
            .id_salt("cmd1.goto.list")
            .auto_shrink([false, false])
            .max_height(height)
            .show_rows(ui, row_h, shown.len(), |ui, range| {
                for k in range {
                    let it = &items[shown[k]];
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    let on = k == st.hi || it.dest == cur;
                    let fg = ddc_ui::widgets::paint_state(ui, &th, rect, &resp, on);
                    let inner = rect.shrink2(egui::vec2(space::SM, 0.0));
                    let kind = ui.painter().text(
                        inner.right_center(),
                        egui::Align2::RIGHT_CENTER,
                        &it.kind,
                        th.font(ddc_ui::theme::Role::Small),
                        th.c("text.secondary"),
                    );
                    let room = (inner.width() - kind.width() - space::MD).max(1.0);
                    let g = ddc_ui::widgets::elide(
                        ui,
                        &it.name,
                        th.font(ddc_ui::theme::Role::Body),
                        fg,
                        room,
                    );
                    let y = inner.center().y - g.size().y / 2.0;
                    ui.painter().galley(egui::pos2(inner.left(), y), g, fg);
                    if k == st.hi && (up || down) {
                        ui.scroll_to_rect(rect, None);
                    }
                    if resp.clicked() {
                        *pick = Some(it.dest);
                    }
                }
            });
    }
    if enter && !shown.is_empty() {
        *pick = Some(items[shown[st.hi]].dest);
    }
    dialog_footer(ui, |ui| {
        let blocked = shown.is_empty().then(|| c.t("view_cmds.goto.none"));
        if primary_click(ui, c.t("view_cmds.goto.open"), blocked.as_deref()) && !shown.is_empty() {
            *pick = Some(items[shown[st.hi]].dest);
        }
        if cancel_button(ui, c.t("find_replace.close")).clicked() {
            *close = true;
        }
    });
}

fn goto_frame(a: &mut EditorApp, ctx: &egui::Context) {
    let mut st = with(|s| std::mem::take(&mut s.goto));
    if !st.open {
        with(|s| s.goto = st);
        return;
    }
    let items = items(&a.ed, &a.sh.cat);
    let cur = current(a);
    let (mut pick, mut close, mut open) = (None, false, true);
    {
        let c = &a.sh.cat;
        Dialog::new("view_cmds.goto", c.t("cmd.view.goto"), DialogSize::Tool)
            .open(&mut open)
            .show(ctx, |ui| goto_body(ui, c, &mut st, &items, cur, &mut pick, &mut close));
    }
    st.open = open && !close && pick.is_none();
    with(|s| s.goto = st);
    if let Some(d) = pick {
        go(a, d);
    }
}

/// Per frame from the shell, after the views were drawn.
pub fn frame(a: &mut EditorApp, ctx: &egui::Context) {
    sync(a, ctx.pointer_latest_pos());
    goto_frame(a, ctx);
}

/// Fresh go-to window for the layout probes.
#[cfg(test)]
pub(crate) fn open_goto_for_layout(query: &str) {
    with(|s| {
        s.goto = GotoUi {
            open: true,
            query: query.into(),
            ..GotoUi::default()
        };
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_ui::Lang;

    fn reset() {
        with(|s| *s = St::default());
    }

    fn app() -> EditorApp {
        reset();
        crate::shot_press::set_screenshot_mode(true);
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        a.prefs.path = None;
        a
    }

    fn wall(a: &mut EditorApp, x0: f64, y0: f64, x1: f64, y1: f64) {
        let spec = ddc_model::WallSpec {
            a: (x0, y0),
            b: (x1, y1),
            level: a.ed.level,
            base_offset: 0.0,
            height: 3.0,
            wall_type: a.ed.types[0].id,
            offset0: None,
        };
        a.ed.model.add_wall(&spec).unwrap();
        a.ed.reload().unwrap();
    }

    #[test]
    fn toggles_flip_and_say_so() {
        let mut a = app();
        assert!(!tabs_always() && !tiled() && !sync_on());
        assert!(a.run("view.tabs"));
        assert!(tabs_always());
        assert_eq!(a.ed.status.as_deref(), Some(crate::tr("view_cmds.tabs.on", &[]).as_str()));
        assert!(a.run("view.tabs"));
        assert!(!tabs_always());
        assert!(a.run("view.tile"));
        assert!(tiled());
        assert!(a.run("view.tile"));
        assert!(!tiled());
        assert!(a.run("view.sync_sel"));
        assert!(sync_on());
        assert_eq!(a.ed.status.as_deref(), Some(crate::tr("view_cmds.sync.on", &[]).as_str()));
    }

    #[test]
    fn section_through_spans_the_model_and_looks_along_the_direction() {
        let bb = Some([0.0, 0.0, 10.0, 6.0]);
        let s = section_through(bb, v2(5.0, 1.0), v2(0.0, 1.0), "S").unwrap();
        let n = crate::editor::sec::look(&s);
        assert!((n.x).abs() < 1e-9 && (n.y - 1.0).abs() < 1e-9, "looks north: {n:?}");
        let (xa, xb) = (s.a.0.min(s.b.0), s.a.0.max(s.b.0));
        assert!(xa <= -1.0 + 1e-9 && xb >= 11.0 - 1e-9, "line spans the model: {s:?}");
        assert!((s.a.1 - 1.0).abs() < 1e-9 && (s.b.1 - 1.0).abs() < 1e-9);
        assert!((s.depth - 6.0).abs() < 1e-9, "far side 5 m ahead + 1 m: {}", s.depth);
        // the model behind the cut: default depth, never below the minimum
        let s = section_through(bb, v2(5.0, 9.0), v2(0.0, 1.0), "S").unwrap();
        assert_eq!(s.depth, crate::editor::sec::DEFAULT_DEPTH);
        // empty model: a 10 m line through the point
        let s = section_through(None, v2(2.0, 3.0), v2(1.0, 0.0), "S").unwrap();
        let (p, q) = crate::editor::sec::ends(&s);
        assert!(((q - p).len() - 10.0).abs() < 1e-9);
        assert!((((p + q) * 0.5) - v2(2.0, 3.0)).len() < 1e-9);
        let n = crate::editor::sec::look(&s);
        assert!((n.x - 1.0).abs() < 1e-9, "looks east: {n:?}");
        assert!(section_through(bb, v2(0.0, 0.0), v2(0.0, 0.0), "S").is_none());
    }

    #[test]
    fn section_here_adds_one_undoable_view_and_opens_it() {
        let mut a = app();
        wall(&mut a, 0.0, 0.0, 8.0, 0.0);
        wall(&mut a, 0.0, 5.0, 8.0, 5.0);
        a.plan.center = v2(4.0, 2.0);
        let before = a.ed.views.len();
        assert!(a.run("view.section_here"));
        assert_eq!(a.ed.views.len(), before + 1);
        let (id, s) = a.ed.views.last().cloned().unwrap();
        assert_eq!(a.section, Some(id));
        assert!(a.tabs.contains(&id));
        assert!(s.a.1 == 2.0 && s.b.1 == 2.0, "through the plan centre: {s:?}");
        assert!(s.depth > 3.0, "reaches the far wall: {}", s.depth);
        // from the open section: across it
        assert!(a.run("view.section_here"));
        let (_, s2) = a.ed.views.last().cloned().unwrap();
        let n = crate::editor::sec::look(&s2);
        assert!(n.y.abs() < 1e-9, "looks along the first section's line: {n:?}");
        a.run("edit.undo");
        a.run("edit.undo");
        assert_eq!(a.ed.views.len(), before);
    }

    #[test]
    fn section_here_in_3d_follows_the_camera() {
        let mut a = app();
        a.view3d = true;
        a.view.camera.target = ddc_render::glam::Vec3::new(3.0, 4.0, 1.0);
        a.view.camera.yaw = std::f32::consts::PI; // eye to the west, looking east
        assert!(a.run("view.section_here"));
        let (_, s) = a.ed.views.last().cloned().unwrap();
        let n = crate::editor::sec::look(&s);
        assert!((n.x - 1.0).abs() < 1e-6 && n.y.abs() < 1e-6, "{n:?}");
        let (p, q) = crate::editor::sec::ends(&s);
        assert!((((p + q) * 0.5).x - 3.0).abs() < 1e-6);
        assert!(!a.view3d && a.section.is_some(), "the new section is opened");
    }

    #[test]
    fn goto_lists_every_view_filters_and_opens() {
        let mut a = app();
        let l2 = a.ed.add_level("Roof", 6.0).unwrap();
        let s = section_through(None, v2(0.0, 0.0), v2(0.0, 1.0), "Cut A").unwrap();
        let sec = a.ed.add_view(&s).unwrap();
        for lang in [Lang::En, Lang::Ru] {
            let c = Catalog::new(lang);
            let it = items(&a.ed, &c);
            assert!(it.iter().any(|i| i.dest == Dest::Plan(l2)));
            assert!(it.iter().any(|i| i.dest == Dest::D3));
            assert!(it.iter().any(|i| i.dest == Dest::View(sec) && i.name == "Cut A"));
            assert!(it.iter().all(|i| !i.kind.is_empty() && !i.kind.starts_with("view_cmds.")));
            let f = filtered(&it, "cut a");
            assert_eq!(f.len(), 1);
            assert_eq!(it[f[0]].dest, Dest::View(sec));
        }
        assert!(a.run("view.goto"));
        assert!(with(|s| s.goto.open));
        go(&mut a, Dest::View(sec));
        assert_eq!(a.section, Some(sec));
        go(&mut a, Dest::Plan(l2));
        assert_eq!((a.ed.level, a.section, a.view3d), (l2, None, false));
        go(&mut a, Dest::D3);
        assert!(a.view3d);
    }

    #[test]
    fn sync_frames_the_plan_on_a_selection_made_elsewhere() {
        let mut a = app();
        wall(&mut a, 100.0, 100.0, 104.0, 100.0);
        let w = a.ed.walls[0].id;
        a.plan.center = v2(0.0, 0.0);
        note_rects(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0)),
            egui::Rect::NOTHING,
        );
        // off: nothing moves
        a.ed.selection = vec![Sel::Wall(w)];
        sync(&mut a, None);
        assert_eq!(a.plan.center, v2(0.0, 0.0));
        a.ed.selection.clear();
        a.run("view.sync_sel");
        // made with the pointer over the plan: the plan stays
        a.ed.selection = vec![Sel::Wall(w)];
        sync(&mut a, Some(egui::pos2(10.0, 10.0)));
        assert_eq!(a.plan.center, v2(0.0, 0.0));
        // made elsewhere (browser, table, palette): the plan frames it
        a.ed.selection.clear();
        sync(&mut a, None);
        a.ed.selection = vec![Sel::Wall(w)];
        sync(&mut a, Some(egui::pos2(900.0, 10.0)));
        assert!((a.plan.center - v2(102.0, 100.0)).len() < 3.0, "{:?}", a.plan.center);
    }

    fn room_app() -> (EditorApp, i64) {
        let mut a = app();
        // the app opens in Select; the fixture draws its four walls with the wall tool
        a.ed.set_mode(crate::wall_tool::Mode::Polyline);
        crate::schedule::test_room(&mut a.ed);
        a.ed.set_mode(crate::wall_tool::Mode::Select);
        if a.ed.rooms.is_empty() {
            let spec = ddc_model::RoomSpec {
                level: a.ed.level,
                name: "R".into(),
                number: "1".into(),
                point: (3.0, 2.0),
                height: 3.0,
            };
            a.ed.model.add_room(&spec).unwrap();
            a.ed.reload().unwrap();
        }
        let id = a.ed.rooms[0].id;
        (a, id)
    }

    #[test]
    fn zoom_sel_frames_a_selected_room_in_the_plan() {
        let (mut a, id) = room_app();
        a.ed.selection = vec![Sel::Room(id)];
        a.plan.center = v2(500.0, 500.0);
        a.plan.scale = 3.0;
        assert!(a.run("view.zoom_sel"));
        let ring = &a.ed.rooms[0].rings[0];
        let (lo, hi) = ring.iter().fold((ring[0], ring[0]), |(lo, hi), p| {
            (v2(lo.x.min(p.x), lo.y.min(p.y)), v2(hi.x.max(p.x), hi.y.max(p.y)))
        });
        assert!((a.plan.center - (lo + hi) * 0.5).len() < 0.5, "{:?}", a.plan.center);
        assert!(a.plan.scale > 10.0, "the room fills the view: {}", a.plan.scale);
        assert_ne!(a.ed.status.as_deref(), Some(crate::tr("v3d.msg.no_sel", &[]).as_str()));
    }

    #[test]
    fn zoom_sel_without_a_selection_says_so_and_keeps_the_view() {
        let (mut a, _) = room_app();
        a.ed.selection.clear();
        a.plan.center = v2(7.0, 8.0);
        assert!(a.run("view.zoom_sel"));
        assert_eq!(a.plan.center, v2(7.0, 8.0));
        assert_eq!(a.ed.status.as_deref(), Some(crate::tr("v3d.msg.no_sel", &[]).as_str()));
    }

    #[test]
    fn next_walks_the_tabs_and_close_closes_the_open_section() {
        let mut a = app();
        wall(&mut a, 0.0, 0.0, 8.0, 0.0);
        a.plan.center = v2(4.0, 2.0);
        assert!(a.run("view.section_here"));
        let first = a.section.unwrap();
        assert!(a.run("view.section_here"));
        let second = a.section.unwrap();
        assert_eq!(view_tabs(&a).0.len(), 4, "plan, 3D and two sections");
        // from the second section the strip wraps to the plan, then 3D, then the sections
        assert!(a.run("view.next"));
        assert_eq!((a.section, a.view3d), (None, false));
        assert!(a.run("view.next"));
        assert!(a.view3d);
        // from 3D the next tab is the first section, which leaves 3D
        assert!(a.run("view.next"));
        assert_eq!((a.section, a.view3d), (Some(first), false));
        assert!(a.run("view.next"));
        assert_eq!(a.section, Some(second));
        // close drops the open one and leaves the other tab
        assert!(a.run("view.close"));
        assert!(!a.tabs.contains(&second) && a.tabs.contains(&first));
        assert_ne!(a.section, Some(second));
        // the plan has nothing to close: it says so and stays
        a.section = None;
        let tabs = a.tabs.clone();
        assert!(a.run("view.close"));
        assert_eq!(a.tabs, tabs);
        assert_eq!(a.ed.status.as_deref(), Some(crate::tr("cmd_buttons.close.base", &[]).as_str()));
    }

    #[test]
    fn tile_frames_the_plan_again() {
        let mut a = app();
        wall(&mut a, 0.0, 0.0, 8.0, 0.0);
        a.plan.center = v2(900.0, 900.0);
        assert!(a.run("view.tile"));
        assert!((a.plan.center - v2(4.0, 0.0)).len() < 1.0, "{:?}", a.plan.center);
    }
}
