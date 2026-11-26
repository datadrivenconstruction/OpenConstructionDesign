// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Shell panels as components: title bar, command surfaces, project browser, tool options,
//! inspector, view area, data table, status bar. `arrange()` decides which ones show and where.
use super::*;

use super::prop_multi;
use crate::opening_tool::Kind;
use super::{prop_head, prop_rows};
use super::tree_ux::{self, Target};
use crate::ai::ui::modes::{self as ai_modes, AiMode};
use ddc_ui::widgets::shell_kit;

#[path = "browser_sec.rs"]
pub(crate) mod browser_sec;
#[path = "browser.rs"]
pub(crate) mod browser;
#[path = "library_win.rs"]
pub(crate) mod library_win;
#[path = "opt_ctx.rs"]
pub(super) mod opt_ctx;
#[path = "param_editor.rs"]
pub(crate) mod param_editor;
#[path = "replace_type.rs"]
pub(crate) mod replace_type;
#[path = "tool_opts.rs"]
mod tool_opts;
#[path = "type_dialog.rs"]
pub(crate) mod type_dialog;
#[path = "type_items.rs"]
pub(crate) mod type_items;
#[path = "type_library.rs"]
pub(crate) mod type_library;
#[path = "wall_type_editor.rs"]
pub(crate) mod wall_type_editor;
use ddc_ui::widgets::type_pick::{Pick, TypePick};

/// Height of the options bar under the command surface.
const OPTIONS_H: f32 = 40.0;

/// XLSX rows of the File menu, in order: (command, menu icon).
const XLSX_ROWS: [(&str, &str); 3] = [
    ("data.export_xlsx", "xlsx-out"),
    ("data.import_xlsx", "xlsx-in"),
    ("data.sync_xlsx", "xlsx-sync"),
];

/// Longest project name in the title bar control for a bar `bar_w` wide; longer ones end in
/// an ellipsis and show in full in the tooltip.
pub(super) fn file_title_max(bar_w: f32) -> f32 {
    let xl = ddc_ui::theme::space::XXXL;
    (bar_w * 0.16).clamp(xl * 2.5, xl * 7.5)
}

impl EditorApp {
    pub(super) fn title_bar(&mut self, ui: &mut egui::Ui, fire: &mut Vec<String>) {
        let full = ui.max_rect();
        {
            let th = &self.sh.theme;
            crate::brand::design_mark(ui, th, 22.0);
            ui.label(
                th.rich(Role::WindowTitle, crate::brand::PRODUCT),
            );
            widgets::vdivider(ui, th, 24.0);
        }
        // project name + save state + chevron: one control that opens the File menu
        let file_resp = {
            let th = &self.sh.theme;
            let (name, dirty) = crate::project_ui::header_label(self);
            // the tooltip says what the control is (the File menu), then the save state
            let menu = self.sh.cat.t("hdr.file_menu");
            let desc = if dirty {
                format!("{menu} \u{b7} {}", self.sh.cat.t("hdr.unsaved"))
            } else {
                menu
            };
            let id = ui.id().with("file.menu");
            let open = egui::Popup::is_id_open(ui.ctx(), id.with("popup"));
            let enabled = !self.project_ui.modal();
            ui.add_enabled_ui(enabled, |ui| {
                let m = shell_kit::MenuTitle {
                    id,
                    name: &name,
                    dirty,
                    open,
                    max_w: file_title_max(full.width()),
                    desc: &desc,
                    keys: "Alt+F",
                };
                shell_kit::menu_title(ui, th, &m)
            })
            .inner
        };
        if !self.project_ui.modal()
            && ui.ctx().input_mut(|i| i.consume_key(egui::Modifiers::ALT, egui::Key::F))
        {
            egui::Popup::toggle_id(ui.ctx(), file_resp.id.with("popup"));
        }
        let row = |id: &str, key: &str| crate::project_ui::menu_row(self, id, key);
        let (r_import, r_settings) = (
            row("file.import", "menu.import"),
            row("mgr.settings", "menu.settings"),
        );
        // the XLSX round trip keeps labelled rows here, each with its description as tooltip
        let xlsx_rows = XLSX_ROWS.map(|(id, icon)| {
            let key = format!("cmd.{id}");
            (id, icon, row(id, key.as_str()), self.sh.cat.t(&format!("{key}.desc")))
        });
        let th = self.sh.theme.clone();
        let file_items = crate::project_ui::menu_items(self);
        let r_start = (
            self.sh.cat.t("menu.start"),
            crate::quick::keys_text(self, "ui.start"),
        );
        use crate::project_ui::kit_row;
        // no second Data menu: XLSX rows sit in File, the rest on the Data and Manage tabs
        egui::Popup::menu(&file_resp).show(|ui| {
            crate::project_ui::file_menu(ui, &th, &file_items, &mut self.project_ui, fire);
            widgets::menu_divider(ui, &th);
            for (id, icon, r, desc) in &xlsx_rows {
                let resp = kit_row(ui, &th, Some(*icon), r);
                if widgets::rich_tooltip(resp, &th, &r.0, desc, "").clicked() {
                    fire.push((*id).into());
                    ui.close();
                }
            }
            widgets::menu_divider(ui, &th);
            // fired, not flagged: frecency and Repeat see menu use like Ctrl+I / Ctrl+,
            if kit_row(ui, &th, Some("import"), &r_import).clicked() {
                fire.push("file.import".into());
                ui.close();
            }
            if kit_row(ui, &th, Some("settings"), &r_settings).clicked() {
                fire.push("mgr.settings".into());
                ui.close();
            }
            widgets::menu_divider(ui, &th);
            if kit_row(ui, &th, None, &r_start).clicked() {
                fire.push("ui.start".into());
                ui.close();
            }
        });
        widgets::vdivider(ui, &self.sh.theme, 24.0);
        let (u, r) = (self.ed.can_undo(), self.ed.can_redo());
        if self.icon_cmd(ui, "edit.undo", false, u) {
            fire.push("edit.undo".into());
        }
        if self.icon_cmd(ui, "edit.redo", false, r) {
            fire.push("edit.redo".into());
        }
        // search in the middle, work-mode group at the right; the group never covers the search
        let left_end = ui.cursor().left();
        let mode = self.mode_text();
        let fit = self.title_fit(ui, &mode, full, left_end);
        let th = &self.sh.theme;
        if let Some(sr) = fit.search {
            let resp = ui.interact(sr, ui.id().with("search"), egui::Sense::click());
            let p = ui.painter();
            let hov = resp.hovered();
            p.rect(
                sr,
                th.radius("md"),
                th.c("bg.panel"),
                egui::Stroke::new(
                    1.0,
                    th.c(if hov {
                        "border.strong"
                    } else {
                        "border.subtle"
                    }),
                ),
                egui::StrokeKind::Inside,
            );
            let ic = egui::Rect::from_center_size(
                sr.left_center() + egui::vec2(16.0, 0.0),
                egui::vec2(16.0, 16.0),
            );
            let sec = th.c("text.secondary");
            p.circle_stroke(
                ic.center() - egui::vec2(1.0, 1.0),
                4.5,
                egui::Stroke::new(1.5, sec),
            );
            p.line_segment(
                [
                    ic.center() + egui::vec2(2.5, 2.5),
                    ic.center() + egui::vec2(6.0, 6.0),
                ],
                egui::Stroke::new(1.5, sec),
            );
            let kx =
                widgets::paint_keycaps(p, th, sr.right_center() - egui::vec2(6.0, 0.0), "Ctrl+K");
            let hint = self.sh.cat.t("app.search");
            let g = p.layout_no_wrap(hint, th.font(Role::Body), sec);
            let clip = egui::Rect::from_min_max(sr.min, egui::pos2(kx - 6.0, sr.max.y));
            p.with_clip_rect(clip).galley(
                egui::pos2(sr.left() + 30.0, sr.center().y - g.size().y / 2.0),
                g,
                sec,
            );
            if widgets::rich_tooltip(
                resp,
                th,
                &self.sh.cat.t("cmd.app.palette"),
                "",
                "Ctrl+K / Ctrl+Shift+P",
            )
            .clicked()
            {
                fire.push("app.palette".into());
            }
        }
        self.title_mode(ui, &mode, fit, left_end, fire);
    }

    /// The browser panel at `width` points, for the layout probe.
    #[cfg(test)]
    pub(crate) fn browser_for_test(&mut self, ctx: &egui::Context, width: f32) {
        self.prefs.browser_width = width;
        self.browser(ctx);
    }

    /// Project browser (left): views and levels.
    pub(super) fn browser(&mut self, ctx: &egui::Context) {
        let fold_keys = self.fold_keys(fold::CMD_BROWSER);
        let max_w = fold::dock_max(
            fold::PANEL_MAX_W,
            super::dock_ui::BROWSER_MIN_W,
            ctx.screen_rect().width(),
        );
        let s = &mut self.sh;
        let th = &s.theme;
        let mut go3d = None;
        let mut open: Option<i64> = None;
        let mut act: Option<(i64, u8)> = None;
        // level: 0 open plan, 1 rename, 2 delete, 3 new
        let mut lact: Option<(i64, u8)> = None;
        let (views, active) = (self.ed.views.clone(), self.section);
        let levels = self.ed.levels();
        let cur_level = self.ed.level;
        let q = browser::with(|b| b.query.clone());
        let searching = !q.trim().is_empty();
        let mut fold = false;
        let mut clear_query = false;
        let mut renamed: Option<(Target, Option<String>)> = None;
        // row of the `browser.row_*` commands, and a row key (Enter, Del) on a focused row
        let mut cur_row: Option<Option<Target>> = None;
        let mut row_key: Option<(Target, egui::Key)> = None;
        let note_row = |r: &egui::Response, t: Option<Target>, cur: &mut Option<Option<Target>>| {
            if r.clicked() || r.secondary_clicked() || r.gained_focus() {
                *cur = Some(t);
            }
        };
        let take_key = |ui: &egui::Ui, r: &egui::Response| -> Option<egui::Key> {
            if !r.has_focus() {
                return None;
            }
            ui.input_mut(|i| {
                [egui::Key::Enter, egui::Key::Delete]
                    .into_iter()
                    .find(|k| i.consume_key(egui::Modifiers::NONE, *k))
            })
        };
        let panel = egui::SidePanel::left("ddc-browser")
            .resizable(true)
            .min_width(super::dock_ui::BROWSER_MIN_W)
            .max_width(max_w)
            .default_width(self.prefs.browser_width.min(max_w))
            .frame(
                egui::Frame::new()
                    .fill(th.c("bg.panel"))
                    .inner_margin(egui::Margin::symmetric(8, 10)),
            )
            .show(ctx, |ui| {
                super::pane_focus::anchor(ui, super::pane_focus::Pane::Browser);
                super::dock_ui::head(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(
                        th.rich(Role::PanelHeader, &s.cat.t("browser.title")),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let tip = s.cat.t("dock.fold");
                        fold = fold::fold_button(ui, th, true, &tip, &fold_keys).clicked();
                    });
                });
                browser::search(ui, &s.cat);
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt("ddc-browser.scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // rows sit edge to edge: one pitch in every section
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let mut row =
                            |ui: &mut egui::Ui, icon: &str, text: &str, right: &str, on: bool| {
                                // one row system for every section; long names end in an ellipsis
                                browser_sec::row(
                                    ui,
                                    th,
                                    &mut s.icons,
                                    Some(icon),
                                    text,
                                    right,
                                    on,
                                    0,
                                    egui::Sense::click(),
                                )
                            };
                        let mut hits = 0usize;
                        let f2 = ui.input(|i| i.key_pressed(egui::Key::F2));
                        let plan_on = !self.view3d
                            && active.is_none()
                            && crate::sheet_view::active().is_none()
                            && crate::drafting_view::active().is_none();
                        let plan_word = s.cat.t("browser.plan");
                        let view3d_label = s.cat.t("cmd.view.3d");
                        let plans: Vec<(i64, String)> = levels
                            .iter()
                            .map(|(id, name, _)| (*id, format!("{plan_word} · {name}")))
                            .filter(|(_, l)| tree_ux::matches(&q, l))
                            .collect();
                        let show3d = tree_ux::matches(&q, &view3d_label);
                        hits += plans.len() + usize::from(show3d);
                        let views_t = s.cat.t("browser.views");
                        let views_n = plans.len() + usize::from(show3d);
                        if (!plans.is_empty() || show3d)
                            && browser_sec::section(ui, th, &views_t, &views_t, Some(views_n), None)
                                .open
                        {
                            for (id, label) in &plans {
                                let on = plan_on && *id == cur_level;
                                let r = row(ui, "plan-view", label, "", on);
                                note_row(&r, None, &mut cur_row);
                                if r.double_clicked() || (r.clicked() && *id == cur_level) {
                                    lact = Some((*id, 0));
                                }
                                level_menu(&r, *id, &mut lact, &s.cat, th);
                            }
                            if show3d {
                                let r = row(ui, "3d-view", &view3d_label, "", self.view3d);
                                note_row(&r, None, &mut cur_row);
                                if r.clicked() {
                                    go3d = Some(true);
                                }
                            }
                        }
                        // sections and elevations: double click opens the view in a tab
                        for (kind, key, icon) in [
                            (ddc_model::ViewKind::Section, "browser.sections", "section"),
                            (
                                ddc_model::ViewKind::Elevation,
                                "browser.elevations",
                                "elevation",
                            ),
                        ] {
                            let list: Vec<&(i64, ddc_model::SectionSpec)> = views
                                .iter()
                                .filter(|(_, v)| {
                                    v.kind == kind
                                        && tree_ux::matches(
                                            &q,
                                            &crate::editor::sec::display_name(v),
                                        )
                                })
                                .collect();
                            if list.is_empty() {
                                continue;
                            }
                            hits += list.len();
                            let head = s.cat.t(key);
                            let n = Some(list.len());
                            if browser_sec::section(ui, th, &head, &head, n, None).open {
                                for (id, v) in list {
                                    if self.chrome.tree.active(Target::View(*id)) {
                                        if let Some(st) = self.chrome.tree.rename.as_mut() {
                                            if let tree_ux::Edit::Done(n) =
                                                tree_ux::rename_row(ui, st)
                                            {
                                                renamed = Some((st.target, n));
                                            }
                                        }
                                        continue;
                                    }
                                    let name = crate::editor::sec::display_name(v);
                                    let on = active == Some(*id) && !self.view3d;
                                    let r = row(ui, icon, &name, "", on);
                                    note_row(&r, Some(Target::View(*id)), &mut cur_row);
                                    if let Some(k) = take_key(ui, &r) {
                                        row_key = Some((Target::View(*id), k));
                                    }
                                    if r.double_clicked() {
                                        open = Some(*id);
                                    }
                                    if f2 && (r.hovered() || r.has_focus()) {
                                        act = Some((*id, 3));
                                    }
                                    r.context_menu(|ui| {
                                        use crate::ctx_menu;
                                        if ctx_menu::item_icon(ui, th, Some("open"), &s.cat.t("browser.open"), ctx_menu::OPEN)
                                            .clicked()
                                        {
                                            open = Some(*id);
                                            ui.close();
                                        }
                                        if ctx_menu::item_icon(ui, th, None, &s.cat.t("browser.rename"), ctx_menu::RENAME)
                                            .clicked()
                                        {
                                            act = Some((*id, 3));
                                            ui.close();
                                        }
                                        if ctx_menu::item_icon(ui, th, Some("flip-hand"), &s.cat.t("browser.flip"), "").clicked() {
                                            act = Some((*id, 1));
                                            ui.close();
                                        }
                                        ddc_ui::widgets::menu_divider(ui, th);
                                        if ctx_menu::item_icon(ui, th, Some("delete"), &s.cat.t("browser.delete"), ctx_menu::DELETE)
                                            .clicked()
                                        {
                                            act = Some((*id, 2));
                                            ui.close();
                                        }
                                    });
                                }
                            }
                        }
                        hits += crate::drafting_view::browser_rows(
                            ui,
                            &mut self.ed,
                            &|n: &str| tree_ux::matches(&q, n),
                            th,
                            &mut row,
                        );
                        let found: Vec<&(i64, String, f64)> = levels
                            .iter()
                            .rev()
                            .filter(|(_, name, _)| tree_ux::matches(&q, name))
                            .collect();
                        hits += found.len();
                        let lev_t = s.cat.t("browser.levels");
                        let lev_add = format!("+ {}", s.cat.t("cmd.mgr.level"));
                        let lev = (!searching || !found.is_empty()).then(|| {
                            browser_sec::section(
                                ui,
                                th,
                                &lev_t,
                                &lev_t,
                                Some(found.len()),
                                Some(&lev_add),
                            )
                        });
                        if lev.as_ref().is_some_and(|l| l.add) {
                            lact = Some((0, 3));
                        }
                        if lev.is_some_and(|l| l.open) {
                            for (id, name, z) in found {
                                if self.chrome.tree.active(Target::Level(*id)) {
                                    if let Some(st) = self.chrome.tree.rename.as_mut() {
                                        if let tree_ux::Edit::Done(n) = tree_ux::rename_row(ui, st)
                                        {
                                            renamed = Some((st.target, n));
                                        }
                                    }
                                    continue;
                                }
                                let mark = crate::editor::levels::elevation_mark(
                                    *z,
                                    self.ed.tool.units,
                                    crate::ui_is_ru(),
                                );
                                let r = row(ui, "level", name, &mark, *id == cur_level);
                                note_row(&r, Some(Target::Level(*id)), &mut cur_row);
                                if let Some(k) = take_key(ui, &r) {
                                    row_key = Some((Target::Level(*id), k));
                                }
                                if r.double_clicked() {
                                    lact = Some((*id, 1));
                                } else if r.clicked() {
                                    lact = Some((*id, 4));
                                }
                                if f2 && (r.hovered() || r.has_focus()) {
                                    lact = Some((*id, 5));
                                }
                                level_menu(&r, *id, &mut lact, &s.cat, th);
                            }
                            if !searching {
                                if browser_sec::add_row(ui, th, &lev_add, "") {
                                    lact = Some((0, 3));
                                }
                            }
                        }
                        if searching {
                            hits += self
                                .ed
                                .model
                                .sheets()
                                .unwrap_or_default()
                                .iter()
                                .filter(|(_, d)| {
                                    tree_ux::matches(&q, &format!("{} {}", d.number, d.name))
                                })
                                .count();
                        }
                        crate::sheet_view::browser(
                            ui,
                            &s.theme,
                            &mut s.icons,
                            &s.cat,
                            &mut self.ed,
                            &q,
                        );
                        let more =
                            browser::lane_c(ui, &s.theme, &mut s.icons, &s.cat, &mut self.ed);
                        browser_sec::end(ui, &s.theme);
                        if crate::empty_kind::browser_search(searching, hits, more).is_some() {
                            clear_query = tree_ux::empty_search(ui);
                        }
                    });
            });
        let held = ctx.input(|i| i.pointer.any_down());
        let width = panel.response.rect.width();
        if let Some(w) = self.chrome.dock.browser_drag.update(width, held) {
            self.prefs.browser_width = w;
            self.prefs.save();
        }
        if fold {
            self.fold_browser();
        }
        if clear_query {
            browser::clear_search();
        }
        if let Some(t) = cur_row {
            self.chrome.tree.row = t;
        }
        match row_key {
            Some((t, egui::Key::Enter)) => self.row_open(t),
            Some((t, _)) => self.row_delete(t),
            None => {}
        }
        if let Some((target, name)) = renamed {
            self.chrome.tree.rename = None;
            match (target, name) {
                (Target::Level(id), Some(n)) => {
                    if let Some(l) = levels.iter().find(|l| l.0 == id) {
                        self.ed.edit_level(id, &n, l.2);
                    }
                }
                (Target::View(id), Some(n)) => {
                    if let Some(mut v) = self.ed.view_spec(id).cloned() {
                        v.name = n;
                        self.ed.edit_view(id, &v);
                    }
                }
                _ => {}
            }
        }
        if let Some(v) = go3d {
            crate::sheet_view::deactivate();
            self.view3d = v;
            self.section = None;
        }
        match lact {
            Some((id, 0)) => {
                crate::sheet_view::deactivate();
                self.ed.set_level(id);
                self.view3d = false;
                self.section = None;
            }
            Some((id, 1)) => self.start_level_edit(Some(id)),
            Some((id, 2)) => {
                self.ed.delete_level(id);
            }
            Some((_, 3)) => self.start_level_edit(None),
            Some((id, 5)) => {
                if let Some(l) = levels.iter().find(|l| l.0 == id) {
                    self.chrome.tree.start(Target::Level(id), &l.1);
                }
            }
            Some((id, _)) => self.ed.set_level(id),
            None => {}
        }
        if let Some(id) = open {
            self.open_view(id);
        }
        match act {
            Some((id, 1)) => {
                if let Some(v) = self.ed.view_spec(id).cloned() {
                    self.ed.edit_view(id, &v.flipped());
                }
            }
            Some((id, 3)) => {
                if let Some(v) = self.ed.view_spec(id) {
                    let name = crate::editor::sec::display_name(v);
                    self.chrome.tree.start(Target::View(id), &name);
                }
            }
            Some((id, _)) => {
                self.close_view(id);
                self.ed.delete_view(id);
            }
            None => {}
        }
    }

    /// Option bar. Select mode shows the selection context (level and hint, or the selection
    /// chip, type and isolate); a tool shows its quiet name, a way back to Select and only its
    /// own options. The tools themselves live on the ribbon, the toolbar and the palette.
    pub(crate) fn tool_bar(&mut self, ui: &mut egui::Ui, t: &Tokens, fire: &mut Vec<String>) {
        let _ = t;
        let avail = ui.available_width();
        let select_on = !self.ed.tool_armed() && !self.ed.opening_mode;
        let active = self.active_command();
        let tool_active = !(select_on && active.is_none());
        let kind = opt_ctx::bar(tool_active, self.ed.selection.len());
        let name = active
            .map(|id| self.label(id))
            .unwrap_or_else(|| self.sh.cat.t("opt.select"));
        let short = name.split(" (").next().unwrap_or(&name).to_string();
        let select_tip = self.sh.cat.t("opt.select_tip");
        let iso_tip = self.tip("mod.isolate");
        let iso_icon = self
            .sh
            .reg
            .get("mod.isolate")
            .map(|c| c.icon.clone())
            .unwrap_or_default();
        // wall options only for walls and selection; other tools have their own option bars
        let wall_opts = self.ed.model_tool().is_none()
            && !crate::interact::sessions::modify::running(&self.ed)
            && self.plan.b.dim.is_none()
            && !crate::annot_text::armed();
        let opening = self.ed.opening_mode.then_some(self.ed.otool.ty.kind);
        let groups = tool_opts::opt_groups(
            self.ed.wall_mode(),
            self.ed.tool.curtain,
            opening,
            wall_opts,
        );
        let mut isolate = false;
        // what does not fit scrolls sideways instead of being cut or growing the bar
        egui::ScrollArea::horizontal()
            .id_salt("ddc.optbar")
            .auto_shrink([true, true])
            // a thin floating bar on overflow: a narrow window reaches every control by mouse
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
            .show(ui, |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let s = &mut self.sh;
                    let ru = s.cat.lang == Lang::Ru;
                    match kind {
                        opt_ctx::Bar::Idle => {
                            opt_ctx::idle(ui, &s.theme, &s.cat, &mut self.ed, avail)
                        }
                        opt_ctx::Bar::Selected => {
                            let iso = (iso_icon.as_str(), iso_tip.as_str());
                            let out = opt_ctx::selected(
                                ui,
                                &s.theme,
                                &mut s.icons,
                                &s.cat,
                                &mut self.ed,
                                avail,
                                iso,
                            );
                            isolate = out.isolate;
                        }
                        opt_ctx::Bar::Tool => {
                            let th = &s.theme;
                            let icons = &mut s.icons;
                            let back = widgets::icon_button(
                                ui,
                                icons,
                                th,
                                "select",
                                false,
                                &select_tip,
                            );
                            if back.clicked() {
                                self.ed.set_mode(Mode::Select);
                                // tools the option bar names since active_command reports them
                                let b = &mut self.plan.b;
                                (b.dist, b.mt, b.ann) = (None, None, None);
                                if self.view3d {
                                    self.view.e3.set_tool(crate::edit3d::Tool::Select);
                                }
                            }
                            tool_opts::name(ui, th, &short);
                            widgets::vdivider(ui, th, th.d().control);
                            let ed = &mut self.ed;
                            tool_opts::show(ui, th, icons, ru, ed, &groups);
                        }
                    }
                    opt_ctx::follow_focus(ui);
                });
            });
        if isolate {
            fire.push("mod.isolate".into());
        }
    }

    pub(super) fn props(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        self.ed.big.lang = Some(self.sh.cat.lang);
        let fold_keys = self.fold_keys(fold::CMD_PROPS);
        let (ed, edit) = (&mut self.ed, &mut self.edit);
        let (th, c) = (&self.sh.theme, &self.sh.cat);
        let mut export = false;
        let mut fold = false;
        super::dock_ui::head(ui, |ui| {
            ui.label(
                th.rich(Role::PanelHeader, &c.t("props.title")),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let tip = c.t("dock.fold");
                fold = fold::fold_button(ui, th, false, &tip, &fold_keys).clicked();
            });
        });
        if fold {
            self.fold_props();
            return;
        }
        // typed without Enter: gone with the selection, or it would look saved on return
        let edited = self.chrome.room_edit.as_ref().map(|b| b.0);
        if !matches!(ed.selection.as_slice(), [Sel::Room(id)] if Some(*id) == edited) {
            self.chrome.room_edit = None;
        }
        if ed.selection.len() > 1 {
            let ru = c.lang == Lang::Ru;
            if !crate::elem::multi::show(ui, ed) {
                props_multi(ui, th, c, ed, &mut self.sh.toasts);
            }
            crate::phase_ui::props(ui, ed, ru);
            ui_kit::hint_footer(ui);
            return;
        }
        let u = ed.tool.units;
        let sel_op = ed.selection.iter().find_map(|s| match s {
            Sel::Opening(id) => ed.opening(*id).copied(),
            _ => None,
        });
        let sel = ed
            .selection
            .iter()
            .find_map(|s| match s {
                Sel::Wall(id) => ed.walls.iter().find(|w| w.id == *id),
                _ => None,
            })
            .cloned();
        if let Some(o) = sel_op {
            let mut cur = match edit {
                Some((id, v)) if *id == o.id => *v,
                _ => [o.width, o.height, o.sill],
            };
            let mut commit = false;
            let (icon, kind) = match o.kind {
                Kind::Door => ("door", c.t("props.door")),
                Kind::Window => ("window", c.t("props.window")),
                Kind::Void => ("window", crate::tr("core_tools.void", &[])),
            };
            let parts = prop_head::header_parts(&kind, None, "", o.id);
            prop_rows::header(ui, th, &mut self.sh.icons, icon, &parts);
            ui_kit::figures(
                ui,
                &[
                    (c.t("props.in_wall"), o.wall.to_string()),
                    (c.t("props.area"), crate::unit_text::area_ui(o.width * o.height)),
                ],
            );
            ui_kit::group_title(ui, &c.t("props.instance"));
            ui_kit::props(ui, "sel", |ui| {
                for (k, name) in ["props.width", "props.height", "props.sill"]
                    .iter()
                    .enumerate()
                {
                    let salt = egui::Id::new(("props.opening", o.id, k));
                    commit |= ui_kit::kv_field(
                        ui,
                        salt,
                        &c.t(name),
                        &mut cur[k],
                        ui_kit::NumKind::Len(u),
                        0.0,
                        50.0,
                        unit_label(u),
                    );
                }
            });
            crate::geom_props::show(ui, ed, "opening", o.id);
            let (mut flip_facing, mut flip_hand) = (false, false);
            if o.kind != Kind::Void {
                ui.horizontal(|ui| {
                    flip_facing = shell_kit::secondary(ui, th, &c.t("opening_grips.flip_facing"))
                        .on_hover_text(c.t("opening_grips.flip_facing.tip"))
                        .clicked();
                    flip_hand = shell_kit::secondary(ui, th, &c.t("opening_grips.flip_hand"))
                        .on_hover_text(c.t("opening_grips.flip_hand.tip"))
                        .clicked();
                });
            }
            if flip_facing || flip_hand {
                crate::grips_opening::flip_selected(ed, flip_hand);
            }
            *edit = Some((o.id, cur));
            if commit && cur != [o.width, o.height, o.sill] {
                ed.set_opening_dims(o.id, cur[0], cur[1], cur[2]);
                *edit = None;
            }
            ui_kit::hint(ui, c.t("props.hint.opening"));
        } else if let Some(w) = sel {
            super::warn::badge(ui, &self.warnings.1, "wall", w.id);
            let len = wall_length(&w);
            let (lt, ht) = (edit_length(len, u), edit_length(w.height, u));
            let buf = match self.wedit.take() {
                Some(b) if b.0 == w.id => b,
                _ => (w.id, lt.clone(), ht.clone()),
            };
            let (mut lbuf, mut hbuf) = (buf.1, buf.2);
            let mut new_type = None;
            let mut edit_type = false;
            let (mut new_len, mut new_h) = (None, None);
            let straight = w.bulge == 0.0 && w.top.is_none();
            let cur = ed.types.iter().find(|t| t.id == w.wall_type);
            let affects = type_affects(ui, ed, w.wall_type);
            let count = affects.map_or(String::new(), |n| c.plural("prop_head.of_type", n));
            let parts = prop_head::header_parts(
                &c.t("props.wall"),
                cur.map(|t| t.name.as_str()),
                &count,
                w.id,
            );
            prop_rows::header(ui, th, &mut self.sh.icons, "wall", &parts);
            ui_kit::figures(
                ui,
                &[
                    (c.t("props.plan_area"), crate::unit_text::area_ui(w.plan_area)),
                    (c.t("props.side_area"), crate::unit_text::area_ui(len * w.height)),
                    (c.t("props.volume"), crate::unit_text::vol_ui(w.volume)),
                ],
            );
            let open = ui_kit::heading_keyed(
                ui,
                &prop_head::section_key("wall", "props.instance"),
                &c.t("props.instance"),
                None,
            );
            let tip = c.t("props.tip_length");
            ui_kit::props(ui, "sel", |ui| {
                ui_kit::kv_widget(ui, &c.t("props.type"), |ui| {
                    let rows = type_items::items(&ed.types, u, c.lang == Lang::Ru);
                    match TypePick::new("sel.wtype", &rows, Some(w.wall_type))
                        .texts(type_items::texts(c.lang == Lang::Ru))
                        .width(ui_kit::WIDGET_W)
                        .show(ui, th)
                    {
                        Some(Pick::Id(id)) => new_type = Some(id),
                        Some(Pick::EditType) => edit_type = true,
                        None => {}
                    }
                });
                if !open {
                    return;
                }
                if straight {
                    new_len = ui_kit::kv_length(ui, &c.t("props.length"), &mut lbuf, u, &tip);
                } else {
                    ui_kit::kv_num(ui, &c.t("props.axis_length"), display_length(len, u));
                }
                new_h = ui_kit::kv_length(ui, &c.t("props.height"), &mut hbuf, u, &tip);
            });
            if edit_type {
                with_type_dialog(ed, type_dialog::Fam::Wall, w.wall_type);
            }
            let (k_thick, k_layers) = (c.t("props.thickness"), c.t("props.layers"));
            let rows = cur.map_or(vec![], |t| {
                vec![
                    (k_thick.as_str(), display_length(t.thickness(), u)),
                    (k_layers.as_str(), t.layers.len().to_string()),
                ]
            });
            if open {
                crate::geom_props::show(ui, ed, "wall", w.id);
                // "Edit type..." lives in the type picker footer
                prop_rows::type_block(ui, th, c, &rows, affects);
            }
            let parse = |s: &str| {
                parse_edit_length(s, u).ok_or_else(|| {
                    parse_length(s.trim(), u).err().map_or_else(
                        || c.t("units.err.invalid"),
                        |e| crate::expr_field::error_text(c, &e),
                    )
                })
            };
            let err = |s: &str| parse(s).err();
            let mut reset = new_type.is_some();
            if let Some(t) = new_type {
                ed.edit_wall(w.id, None, Some(t), None);
            }
            if let Some(v) = new_len {
                ed.edit_wall(w.id, Some(v), None, None);
                reset = true;
            }
            if let Some(v) = new_h {
                ed.edit_wall(w.id, None, None, Some(v));
                reset = true;
            }
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            for b in [&lbuf, &hbuf] {
                if let (true, Some(e)) = (enter && new_len.is_none() && new_h.is_none(), err(b)) {
                    ed.status = Some(c.tf("units.err.kept", &[("reason", e.as_str())]));
                    reset = true;
                }
            }
            if !reset {
                self.wedit = Some((w.id, lbuf, hbuf));
            }
            ui_kit::hint(ui, c.t("props.hint.wall"));
        } else if let Some(r) = ed.selection.iter().find_map(|s| match s {
            Sel::Room(id) => ed.rooms.iter().find(|r| r.id == *id).cloned(),
            _ => None,
        }) {
            let parts = prop_head::header_parts(&c.t("props.room"), None, "", r.id);
            prop_rows::header(ui, th, &mut self.sh.icons, "room", &parts);
            super::warn::badge(ui, &self.warnings.1, "room", r.id);
            let q = ed.model.room_quantities(r.id).ok().flatten();
            if let Some(q) = &q {
                ui_kit::figures(
                    ui,
                    &[
                        (c.t("props.area"), crate::unit_text::area_ui(q.area)),
                        (c.t("props.perimeter"), display_length(q.perimeter, ed.tool.units)),
                    ],
                );
            }
            if ui_kit::heading(ui, &c.t("props.instance")) {
                let rev = ed.revision;
                let (_, _, mut num, mut name) = match self.chrome.room_edit.take() {
                    Some(b) if b.0 == r.id && b.1 == rev => b,
                    _ => (r.id, rev, r.number.clone(), r.name.clone()),
                };
                let tip = c.t("props.tip_text");
                let (mut new_num, mut new_name) = (None, None);
                ui_kit::props(ui, "room", |ui| {
                    new_num = ui_kit::kv_edit(ui, &c.t("props.number"), &mut num, "", &tip);
                    new_name = ui_kit::kv_edit(ui, &c.t("props.name"), &mut name, "", &tip);
                });
                crate::geom_props::show(ui, ed, "room", r.id);
                if new_num.is_some() || new_name.is_some() {
                    let (n, m) = (new_num.unwrap_or(num), new_name.unwrap_or(name));
                    ed.edit_room(r.id, move |s| {
                        s.number = n;
                        s.name = m;
                    });
                } else {
                    self.chrome.room_edit = Some((r.id, rev, num, name));
                }
            }
        } else if let Some(b) = ed.selection.iter().find_map(|s| match s {
            Sel::Body(k, id) => ed
                .bodies
                .iter()
                .find(|b| b.kind == *k && b.id == *id)
                .cloned(),
            _ => None,
        }) {
            use crate::editor::bodies::BodyKind as K;
            let cat = if b.category.is_empty() {
                "element"
            } else {
                b.category.as_str()
            };
            let title = match b.kind {
                K::Mesh => c.tf("props.imported", &[("cat", cat)]),
                K::Column => c.t("props.column"),
                K::Beam => c.t("props.beam"),
                K::Ceiling => c.t("props.ceiling"),
            };
            let icon = match b.kind {
                K::Column => "column",
                K::Beam => "beam",
                K::Ceiling => "ceiling",
                K::Mesh => "component",
            };
            ui_kit::element_header(ui, icon, &title, "", b.id);
            {
                ui_kit::group_title(ui, &c.t("props.instance"));
                ui_kit::props(ui, "imported", |ui| {
                    ui_kit::kv_num(ui, &c.t("props.name"), b.name.clone());
                });
                match b.kind {
                    K::Mesh => crate::geom_props::mesh_block(ui, ed, &b),
                    _ => crate::geom_props::show(ui, ed, b.kind.table(), b.id),
                }
                crate::imported_props::show(ui, th, ed);
            }
            ui_kit::hint(ui, c.t("props.hint.body"));
        } else if !crate::elem::stairs::props(ui, ed)
            && !crate::elem::fitting::props(ui, ed)
            && !crate::elem::show(ui, ed)
            && !crate::cw_props::show(ui, t, ed, &mut self.cwedit)
            && !crate::mep3d::props(ui, ed)
            && !crate::elem::mep::comp_props(ui, ed)
            && !crate::struct_props::show(ui, &self.sh.cat, ed)
            && !crate::solid_tools::props(ui, t, ed, &self.sh.cat)
            && !crate::geom_props::show_focus(ui, ed)
        {
            if props_empty_has_model_ui(ui, ed) {
                if ui_kit::empty_state_action(ui, "props_model", "empty.props_model.action") {
                    export = true;
                }
            } else if ui_kit::empty_state(ui, "props") {
                ed.set_mode(Mode::Polyline);
            }
        }
        if export {
            self.xlsx_export();
        }
        let ru = self.sh.cat.lang == Lang::Ru;
        crate::phase_ui::props(ui, &mut self.ed, ru);
        crate::origin_ui::props(ui, &mut self.ed, ru);
        crate::value_src_ui::bulk_bar(ui, &mut self.ed, ru);
        crate::params_ui::props(ui, &mut self.ed, ru);
        // whole-model totals only when nothing is selected: a selection shows its own figures
        if self.ed.selection.is_empty() {
            ui.add_space(8.0);
            if ui_kit::heading(ui, &self.sh.cat.t("props.model")) {
                self.model_totals(ui, t);
            }
        }
        ui_kit::hint_footer(ui);
    }

    fn bar(fill: egui::Color32, x: i8) -> egui::Frame {
        egui::Frame::new()
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(x, 0))
    }

    pub(super) fn panel_title(&mut self, ctx: &egui::Context, fire: &mut Vec<String>) {
        egui::TopBottomPanel::top("ddc.title")
            .frame(Self::bar(self.sh.theme.c("bg.app"), 10))
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| self.title_bar(ui, fire));
            });
    }

    /// The ribbon in `mode`: full, the tab strip, or the hidden ribbon's handle. Its height
    /// moves between the modes; what does not fit yet is clipped by the panel.
    pub(super) fn panel_ribbon(
        &mut self,
        ctx: &egui::Context,
        wc: Width,
        mode: fold::RibbonMode,
        fire: &mut Vec<String>,
    ) {
        self.sh.ribbon.small = self.prefs.ribbon_small;
        self.sh.ribbon.toggle_keys = self.fold_keys(fold::CMD_RIBBON);
        if mode == fold::RibbonMode::Hidden {
            self.sh.ribbon.collapsed = self.prefs.ribbon_collapsed;
            self.sh.ribbon.peek = false;
        } else {
            self.chrome.fold.reveal = false;
        }
        let calm = fold::calm(&self.sh.theme, ctx);
        let target = mode.height(widgets::ribbon::TAB_H, self.sh.ribbon.body_height(wc));
        let rh = fold::anim_to(ctx, egui::Id::new("ddc.ribbon.h"), target, calm);
        egui::TopBottomPanel::top("ddc.ribbon")
            .frame(Self::bar(self.sh.theme.c("bg.panel"), 6))
            .exact_height(rh)
            .show(ctx, |ui| {
                if mode == fold::RibbonMode::Hidden {
                    if (rh - target).abs() <= 0.5 {
                        self.ribbon_handle(ui);
                    }
                    return;
                }
                let act = self.active_command();
                let s = &mut self.sh;
                s.ribbon.trailing_w = super::ui_mode::badge_w(ui, &s.theme, &s.cat);
                if let Some(id) = s
                    .ribbon
                    .show(ui, &s.theme, &s.cat, &s.reg, &mut s.icons, act)
                {
                    fire.push(id);
                }
                if let Some(r) = s.ribbon.trailing {
                    super::ui_mode::badge_in(ui, &s.theme, &s.cat, r);
                }
            });
        if mode == fold::RibbonMode::Hidden && self.chrome.fold.reveal {
            self.ribbon_reveal(ctx, fire);
        }
        if self.sh.ribbon.small != self.prefs.ribbon_small
            || self.sh.ribbon.collapsed != self.prefs.ribbon_collapsed
        {
            self.prefs.ribbon_small = self.sh.ribbon.small;
            self.prefs.ribbon_collapsed = self.sh.ribbon.collapsed;
            self.prefs.save();
        }
    }

    /// Docked tool options strip under the command surface.
    pub(super) fn panel_tool_options(
        &mut self,
        ctx: &egui::Context,
        t: &Tokens,
        on: bool,
        fire: &mut Vec<String>,
    ) {
        let Some(h) = self.bar_height(ctx, "ddc.context.fold", on, OPTIONS_H) else {
            return;
        };
        egui::TopBottomPanel::top("ddc.context")
            .frame(Self::bar(self.sh.theme.c("bg.panel"), 10))
            .exact_height(h)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| self.tool_bar(ui, t, fire))
            });
    }

    pub(super) fn panel_status(&mut self, ctx: &egui::Context, on: bool) {
        let full = super::status_bar::BAR_H;
        let Some(h) = self.bar_height(ctx, "ddc.status.fold", on, full) else {
            return;
        };
        egui::TopBottomPanel::bottom("ddc.status")
            .frame(Self::bar(self.sh.theme.c("bg.app"), 0))
            .show_separator_line(false)
            .exact_height(h)
            .show(ctx, |ui| self.status_line(ui));
    }

    /// Height of a bar folding or unfolding to `on`; `None` once it is folded.
    fn bar_height(&self, ctx: &egui::Context, id: &str, on: bool, full: f32) -> Option<f32> {
        let calm = fold::calm(&self.sh.theme, ctx);
        let k = fold::anim(ctx, egui::Id::new(id), on, calm);
        (k > 0.0).then_some((full * k).max(1.0))
    }

    /// Side docks in `order`: vertical toolbar, project browser, inspector. A panel shows only
    /// when `arrange()` placed it expanded; rails and hidden docks are skipped.
    pub(super) fn panel_docks(
        &mut self,
        ctx: &egui::Context,
        ws: &Workspace,
        pl: &workspace::Placement,
        t: &Tokens,
        shown: fold::Shown,
        fire: &mut Vec<String>,
    ) {
        let mut docks = ws.docks.clone();
        docks.sort_by_key(|d| d.order);
        // only a parameter row drawn in this pass may take Ctrl+L: a hidden panel leaves none
        prop_rows::lock_hot_clear();
        for d in docks.into_iter().filter(|_| shown.docks) {
            let placed = pl.rects.iter().any(|(p, _)| *p == d.panel);
            if !placed {
                continue;
            }
            let rail = pl.collapsed.contains(&d.panel);
            match d.panel {
                PanelId::CommandSurface if ws.surface == Surface::Toolbar && !rail => {
                    self.panel_toolbar(ctx, d.size, fire)
                }
                PanelId::Browser => self.dock_browser(ctx, rail),
                PanelId::Inspector => self.dock_props(ctx, t, rail || shown.props_auto),
                _ => {}
            }
        }
        browser::windows(ctx, self);
        crate::sheet_view::windows(ctx, self);
        crate::deliver::ui::frame(ctx, self);
        crate::family_editor::windows(ctx, self);
        if shown.docks {
            crate::help::refdock::dock(self, ctx);
        }
    }

    /// Vertical icon column (layout 3): tools, edit and view commands.
    fn panel_toolbar(&mut self, ctx: &egui::Context, width: f32, fire: &mut Vec<String>) {
        egui::SidePanel::left(PanelId::CommandSurface.egui_id())
            .resizable(false)
            .exact_width(width)
            .frame(
                egui::Frame::new()
                    .fill(self.sh.theme.c("bg.panel"))
                    .inner_margin(egui::Margin::symmetric(6, 8)),
            )
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    let select_on = !self.ed.tool_armed() && !self.ed.opening_mode;
                    let tip = self.sh.cat.t("opt.select_tip");
                    let s = &mut self.sh;
                    if widgets::icon_button(ui, &mut s.icons, &s.theme, "select", select_on, &tip)
                        .clicked()
                    {
                        self.ed.set_mode(Mode::Select);
                    }
                    for id in TOOLS {
                        let on = self.active_command() == Some(id);
                        if self.icon_cmd(ui, id, on, true) && !on {
                            fire.push(id.into());
                        }
                    }
                    ui.separator();
                    let (u, r) = (self.ed.can_undo(), self.ed.can_redo());
                    for (id, en) in [("edit.undo", u), ("edit.redo", r), ("view.zoom_fit", true)] {
                        if self.icon_cmd(ui, id, false, en) {
                            fire.push(id.into());
                        }
                    }
                    ui.separator();
                    for (id, on) in [("view.plan", !self.view3d), ("view.3d", self.view3d)] {
                        if self.icon_cmd(ui, id, on, true) {
                            fire.push(id.into());
                        }
                    }
                });
            });
    }

    /// Interface 5 schedule: category tabs, sorting, filters, editing, totals, export.
    pub(super) fn panel_table(&mut self, ctx: &egui::Context, height: f32) {
        self.table.ru = self.sh.cat.lang == Lang::Ru;
        self.table.sync(&self.ed);
        let u = self.ed.tool.units;
        let th = &self.sh.theme;
        let mut acts = vec![];
        let start_h = if self.prefs.table_height > 0.0 {
            self.prefs.table_height
        } else {
            height
        };
        let panel = egui::TopBottomPanel::bottom(PanelId::DataTable.egui_id())
            .resizable(true)
            .default_height(start_h)
            .min_height(PanelId::DataTable.min_size().y)
            .frame(
                egui::Frame::new()
                    .fill(th.c("bg.panel"))
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ctx, |ui| {
                acts = self.table.show(ui, th, &self.ed.selection, u);
            });
        let held = ctx.input(|i| i.pointer.any_down());
        let h = panel.response.rect.height();
        if let Some(h) = self.chrome.dock.table_drag.update(h, held) {
            self.prefs.table_height = h;
            self.prefs.save();
        }
        crate::clip_tools::note_table(panel.response.rect);
        if std::mem::take(&mut self.table_copy) {
            acts.push(crate::schedule::ui::Action::Copy);
        }
        if let Some(t) = self.table_paste.take() {
            acts.push(crate::schedule::ui::Action::Paste(t));
        }
        if crate::clip_tools::table_focused()
            && !ctx.wants_keyboard_input()
            // Ctrl+Alt+D is "show draft": consume_key alone would ignore the Alt
            && ctx.input_mut(|i| !i.modifiers.alt && i.consume_key(egui::Modifiers::COMMAND, egui::Key::D))
        {
            acts.push(crate::schedule::ui::Action::Fill);
        }
        use crate::schedule::ui::Action;
        for a in acts {
            match a {
                Action::Select(s) => self.ed.selection = s,
                Action::Edit(r, c, text) => {
                    if let Some(tb) = self.table.table.take() {
                        crate::schedule::apply_edit(&mut self.ed, &tb, r, c, &text);
                        self.table.table = Some(tb);
                    }
                }
                Action::Paste(t) => {
                    let (msg, warn) = self.table.paste(&mut self.ed, &t);
                    self.sh.toasts.push(sch_level(warn), msg, None);
                }
                Action::Fill => {
                    let (msg, warn) = self.table.fill_down(&mut self.ed, u);
                    self.sh.toasts.push(sch_level(warn), msg, None);
                }
                Action::Import(path) => {
                    if let Some((msg, warn)) = self.table.import_read(&self.ed, &path, u) {
                        self.sh.toasts.push(sch_level(warn), msg, None);
                    }
                }
                Action::ImportApply => {
                    let (msg, warn) = self.table.import_apply(&mut self.ed, u);
                    self.sh.toasts.push(sch_level(warn), msg, None);
                }
                Action::Copy => {
                    let n = self.table.copy_rows().len();
                    ctx.copy_text(self.table.tsv(u));
                    self.sh
                        .toasts
                        .push(
                            Level::Info,
                            crate::tr("type_dialog.panels.rows_copied", &[("n", n.to_string().as_str())]),
                            None,
                        );
                }
                Action::Export(path) => {
                    let msg = match self.table.table.as_ref().map(|tb| {
                        crate::schedule::export_xlsx_keyed(
                            &self.ed.model,
                            tb,
                            &self.table.ord,
                            &self.table.visible_cols(),
                            u,
                            &path,
                            self.table.group_lines(),
                        )
                    }) {
                        Some(Ok(())) => (
                            Level::Info,
                            crate::tr("type_dialog.panels.saved", &[("path", path.display().to_string().as_str())]),
                        ),
                        Some(Err(e)) => (
                            Level::Warning,
                            crate::tr("type_dialog.panels.not_saved", &[("e", e.to_string().as_str())]),
                        ),
                        None => (Level::Warning, crate::tr("type_dialog.panels.not_saved_empty", &[])),
                    };
                    self.sh.toasts.push(msg.0, msg.1, None);
                }
            }
        }
    }

    fn prepare_3d(&mut self, rs: Option<&RenderState>, t: &Tokens) {
        // 3D has its own view stage; temporary hide and "hidden in view" are shared with the plan
        let mut vis = self.plan.b.vis.clone();
        vis.phase = crate::phase_ui::layer_3d(&self.ed);
        vis.gfx = crate::view_gfx_ui::layer(&self.ed, ddc_model::ViewKey::ThreeD);
        vis.range = None;
        self.view.nav.gfx_hidden.clone_from(&vis.gfx.nav_hidden);
        crate::ai::preview::note_3d();
        crate::ai::preview::hide_into(&mut vis);
        let vkey = vis.key();
        let prev =
            crate::mass_tools::preview_rev(&self.ed).wrapping_add(crate::ai::preview::rev() << 32);
        self.ed.big.lang = Some(self.sh.cat.lang);
        // the realistic style draws plants at fine detail: switching it rebuilds the components
        let fine = self.view.nav.style == ddc_render::VisualStyle::Realistic;
        let restyled = self.ed.big.plants_fine != fine;
        self.ed.big.plants_fine = fine;
        let full = self.scene_rev != self.ed.revision
            || restyled
            || self.scene_vis != vkey
            || self.clash.dirty3d
            || self.host_base.is_none();
        let links_rev = self.links.rev3d();
        let relink = self.scene_links != links_rev;
        if full || relink || self.scene_prev != prev {
            // base scene before 3D view decorations (box fills, styles): shape previews
            // are layered on it, not on the `view3d` result
            let (mut meshes, inst) = if full || relink {
                let reuse = if full {
                    None
                } else {
                    self.host_base.as_ref().map(crate::links_ui::HostBase::scene)
                };
                let sc = match reuse {
                    Some(sc) => sc,
                    None => {
                        // editor and networks (`mep3d`) + `edit3d::scene3d` providers
                        let (sc, ext) = crate::edit3d::scene3d::compose_with(
                            &mut self.ed,
                            &mut self.view.e3.cache,
                        );
                        self.scene_shown = None;
                        self.clash.dirty3d = false;
                        let sc = crate::edit3d::scene3d::filter(&vis, &self.ed, sc, &ext);
                        let sc = crate::view_gfx_ui::paint_scene(&vis.gfx, &self.ed, sc, &ext);
                        self.view.e3.set_ext(ext);
                        self.host_base = Some(crate::links_ui::HostBase::of(&sc));
                        sc
                    }
                };
                // linked models: each link has its own groups, greyed out, not selectable
                let sc = crate::links_ui::attach(&self.links, &self.ed, sc);
                self.scene_links = links_rev;
                let b = self.clash.decorate(sc);
                (b.meshes, b.instanced)
            } else {
                let b = self.view.nav.deco.base_scene();
                b.map(|b| (b.meshes.clone(), b.instanced.clone()))
                    .unwrap_or_default()
            };
            crate::mass_tools::apply_preview(&self.ed, &mut meshes, &mut self.scene_shown);
            crate::ai::preview::apply(&self.ed, &mut meshes);
            // the GPU and the selection index are updated by `view3d::sync` from the final scene
            self.scene = ddc_render::Scene::shared(meshes).with_instanced(inst);
            self.scene_rev = self.ed.revision;
            self.scene_vis = vkey;
            self.scene_prev = prev;
            if full {
                self.view.refit();
            }
        }
        if let Some(b) = self.clash.fit.take() {
            self.view.camera.fit(&b, 1.6);
        }
        // snap switch and family profile reach both views even without the status bar
        super::status_bar::sync(self);
        // the 3D view extends the base scene and hands the result to the GPU and selection index itself
        crate::view3d::sync(self, rs, t);
        self.ed.axes_pref = self.prefs.full_axes;
        let (view, ed) = (&mut self.view, &self.ed);
        // a clash or a picked face comes first; otherwise the whole selection, cached
        match self
            .clash
            .highlight()
            .or_else(|| crate::mass_tools::picked_range(ed))
        {
            Some(r) => view.highlight_one(Some(r)),
            None => {
                let set = view.sel_cache.ranges(ed);
                view.highlight_set(ed.highlight(), set);
            }
        }
        view.hover_range = view.hovered.and_then(|e| {
            crate::mass_tools::hover_range(e)
                .or_else(|| ed.sel_of_element(e).and_then(|s| ed.element_range(s)))
        });
        view.accent = [t.accent.r(), t.accent.g(), t.accent.b()];
        view.reveal = self.reveal.as_mut().map(|p| p.current());
        view.follow = self.reveal.as_ref().and_then(|p| p.follow_data());
        view.dark = self.sh.theme.mode == ddc_ui::Mode::Dark;
        view.bg = crate::report_shot::paper().map_or_else(
            || [self.sh.theme.c("canvas.bg.top"), self.sh.theme.c("canvas.bg.bottom")],
            |c| [c, c],
        );
        crate::gfx_ui::sync(self, rs);
    }

    /// View area: one view, or plan | 3D side by side (layout 3 above the tabs breakpoint);
    /// an open section/elevation replaces the plan (single) or the 3D half (split). Open views
    /// get a tab strip on top. Returns the view rectangle for overlays.
    pub(super) fn panel_views(
        &mut self,
        ctx: &egui::Context,
        rs: Option<&RenderState>,
        ws: &Workspace,
        pl: &workspace::Placement,
        t: &Tokens,
        fire: &mut Vec<String>,
    ) -> egui::Rect {
        let ids: Vec<i64> = self.ed.views.iter().map(|v| v.0).collect();
        self.tabs.retain(|x| ids.contains(x));
        if self.section.is_some_and(|x| !ids.contains(&x)) {
            self.section = None;
        }
        // an open sheet fills the whole view area; a sheet tab has a negative id
        let sheet = crate::sheet_view::active();
        let drafting = crate::drafting_view::current(&self.ed).filter(|_| sheet.is_none());
        // an AI tab: its work column and a preview of the plan, 3D or sheet take the area
        let ai = ai_modes::sync(ai_modes::base_of(self));
        // plan and section views show and snap to the 3D work plane
        crate::wp_views::sync(self);
        // view panes take the area unless a sheet, a drafting view or an AI tab has it
        let panes_on = self.panes.on && sheet.is_none() && drafting.is_none() && ai.is_none();
        if panes_on {
            self.panes.check(&self.ed);
            if let Some(i) = self.panes.main_pane() {
                self.view.nav.style = self.panes.panes[i].style;
            }
        }
        let split = match ws.views {
            _ if panes_on => None,
            workspace::ViewArrangement::SplitPlan3d { ratio, .. }
                if !pl.view_tabs && sheet.is_none() && drafting.is_none() && ai.is_none() =>
            {
                Some(ratio)
            }
            // view.tile: plan beside 3D or the open section in any layout
            _ if crate::view_cmds::tiled() && sheet.is_none() && drafting.is_none() => {
                Some(0.5)
            }
            _ => None,
        };
        let section = self.section.filter(|_| !self.view3d);
        let ai_3d = ai.is_some_and(|a| a.1 == ai_modes::Preview::D3);
        let panes_3d = panes_on && self.panes.shows_3d();
        if panes_3d || (!panes_on && (self.view3d || (split.is_some() && section.is_none())))
            || ai_3d
        {
            self.prepare_3d(rs, t);
        }
        let mut titles: Vec<(i64, String)> = self
            .tabs
            .iter()
            .filter_map(|id| {
                self.ed
                    .view_spec(*id)
                    .map(|v| (*id, crate::editor::sec::display_name(v)))
            })
            .collect();
        if let Some(d) = crate::drafting_view::active() {
            if let Some(name) = crate::drafting_view::title(&self.ed, d) {
                titles.push((crate::drafting_view::tab_id(d), name));
            }
        }
        if let Some(s) = sheet.and_then(|s| self.ed.model.sheet(s).ok().map(|d| (s, d))) {
            titles.push((
                -s.0,
                self.sh.cat.tf(
                    "sheets.tab",
                    &[("number", &s.1.number), ("name", &s.1.name)],
                ),
            ));
        }
        let cur = match sheet {
            // the panes mark the tab of the active pane
            None if panes_on => match self.panes.active_src() {
                crate::view_panes::Src::D3(_) => Tab::D3,
                crate::view_panes::Src::View(id) => Tab::View(id),
                _ => Tab::Plan,
            },
            Some(s) => Tab::View(-s),
            None => match drafting {
                Some(d) => Tab::View(crate::drafting_view::tab_id(d)),
                None if self.view3d => Tab::D3,
                None => section.map_or(Tab::Plan, Tab::View),
            },
        };
        let ru = self.sh.cat.lang == Lang::Ru;
        let line = self.sh.theme.c("border.strong");
        let plan_title = format!("{} · {}", crate::tr_str(ru, "type_dialog.panels.plan"), self.ed.level_name());
        let (plan, view, scene, ed, sview, panes) = (
            &mut self.plan,
            &mut self.view,
            &self.scene,
            &mut self.ed,
            &mut self.sview,
            &mut self.panes,
        );
        let cat = &self.sh.cat;
        let ai_fold = self.prefs.fold.ai;
        let mut act = None;
        let mut plan_rect = egui::Rect::NOTHING;
        let mut d3_rect = egui::Rect::NOTHING;
        let mut compass = false;
        let mut ai_lay = None;
        let area = ui_kit::viewport_panel(ctx, t, |ui| {
            // the bar is always there: it carries the AI tabs
            let (r, a) = view_tabs(ui, t, cat, &plan_title, &titles, cur, ai.map(|a| a.0));
            act = a;
            if let Some((_, preview, ratio)) = ai {
                let lay = ai_modes::layout(r, ratio, ai_fold);
                ai_lay = Some(lay);
                let v = lay.view;
                if !v.is_positive() {
                    return;
                }
                ui.scope_builder(egui::UiBuilder::new().max_rect(v), |ui| {
                    ui.set_clip_rect(v);
                    match preview {
                        ai_modes::Preview::Sheet if sheet.is_some() => {
                            crate::sheet_view::show(ui, ed, t, ru);
                        }
                        ai_modes::Preview::D3 => {
                            crate::edit3d::show(ui, view, scene, ed);
                            crate::topo::overlay3d(ed, view, ui);
                        }
                        _ => {
                            plan.show(ui, ed, t);
                        }
                    }
                });
                return;
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| {
                ui.set_clip_rect(r);
                if panes_on {
                    let mut cx = crate::view_panes::Cx {
                        panes: &mut *panes,
                        plan: &mut *plan,
                        view: &mut *view,
                        scene,
                        ed: &mut *ed,
                        t,
                        cat,
                    };
                    let o = crate::view_panes::show(ui, r, &mut cx);
                    (plan_rect, d3_rect, compass) = (o.plan, o.d3, o.compass);
                } else if let Some(ratio) = split {
                    let x = r.min.x + r.width() * ratio.clamp(0.2, 0.8);
                    let left = egui::Rect::from_min_max(r.min, egui::pos2(x - 1.0, r.max.y));
                    let right = egui::Rect::from_min_max(egui::pos2(x + 1.0, r.min.y), r.max);
                    plan_rect = left;
                    ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                        ui.set_clip_rect(left);
                        plan.show(ui, ed, t)
                    });
                    ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                        ui.set_clip_rect(right);
                        match section {
                            Some(id) => sview.show(ui, ed, id, t, &plan.b.vis, &mut plan.b.ms),
                            None => {
                                d3_rect = right;
                                crate::edit3d::show(ui, view, scene, ed);
                                crate::topo::overlay3d(ed, view, ui);
                            }
                        }
                    });
                    ui.painter()
                        .vline(x, r.y_range(), egui::Stroke::new(2.0, line));
                } else if sheet.is_some() {
                    crate::sheet_view::show(ui, ed, t, ru);
                } else if let Some(d) = drafting {
                    crate::drafting_view::show(ui, ed, d, t, &mut plan.b);
                } else if cur == Tab::D3 {
                    d3_rect = r;
                    crate::edit3d::show(ui, view, scene, ed);
                    crate::mass_tools::keys_3d(ui, ed);
                    crate::topo::overlay3d(ed, view, ui);
                } else if let Some(id) = section {
                    sview.show(ui, ed, id, t, &plan.b.vis, &mut plan.b.ms);
                } else {
                    plan_rect = r;
                    plan.show(ui, ed, t);
                }
            });
        });
        if act.is_none() {
            use crate::section_hud::SvAct;
            act = match self.sview.act.take() {
                Some(SvAct::ShowOnPlan) => Some(Tab::Plan),
                Some(SvAct::Close) => section.map(Tab::Close),
                None => None,
            };
        }
        if let Some(lay) = ai_lay {
            crate::ai::ui::modes_view::workspace(self, ctx, &lay);
        }
        // a view tab leaves the AI tab even when it names the view under it
        if matches!(act, Some(Tab::Plan | Tab::D3 | Tab::View(_))) {
            ai_modes::leave();
        }
        if panes_on {
            use crate::view_panes::Src;
            // a view tab goes to its pane, or into the active pane
            match act {
                Some(Tab::Plan) => self.panes.focus(Src::Plan(self.ed.level)),
                Some(Tab::D3) => self.panes.focus(Src::D3(self.panes.main)),
                Some(Tab::View(id)) if id > 0 && self.ed.view_spec(id).is_some() => {
                    self.panes.focus(Src::View(id))
                }
                _ => {}
            }
            if matches!(act, Some(Tab::Plan | Tab::D3 | Tab::View(_))) {
                act = None;
            }
            if compass {
                self.ed.add_compass();
                self.panes.check(&self.ed);
            }
            // tools and keys follow the active pane; the 3D pane keeps its own style
            self.view3d = matches!(self.panes.active_src(), Src::D3(_));
            if let Some(i) = self.panes.main_pane() {
                self.panes.panes[i].style = self.view.nav.style;
            }
        }
        match act {
            Some(Tab::Ai(m)) => ai_modes::enter(self, m),
            Some(Tab::View(id) | Tab::Close(id)) if crate::drafting_view::of_tab(id).is_some() => {
                if matches!(act, Some(Tab::Close(_))) {
                    crate::drafting_view::deactivate();
                } else if let Some(d) = crate::drafting_view::of_tab(id) {
                    crate::drafting_view::open(d);
                }
            }
            Some(Tab::View(id) | Tab::Close(id)) if id < 0 => {
                if matches!(act, Some(Tab::Close(_))) {
                    crate::sheet_view::deactivate();
                }
            }
            Some(Tab::Plan) => {
                crate::sheet_view::deactivate();
                crate::drafting_view::deactivate();
                self.view3d = false;
                self.section = None;
            }
            Some(Tab::D3) => {
                crate::sheet_view::deactivate();
                crate::drafting_view::deactivate();
                self.view3d = true;
            }
            Some(Tab::View(id)) => self.open_view(id),
            Some(Tab::Close(id)) => self.close_view(id),
            Some(Tab::CloseSet(kind, anchor)) => {
                let order: Vec<i64> = titles.iter().map(|v| v.0).collect();
                let set = tab_strip::close_set(kind, &order, anchor);
                let was_open = self.section;
                for id in &set {
                    self.close_tab_id(*id);
                }
                // the tab the menu was opened on takes over when the open one went away
                if let (Some(a), Some(s)) = (anchor, was_open) {
                    if set.contains(&s) && self.tabs.contains(&a) {
                        self.open_view(a);
                    }
                }
            }
            None => {}
        }
        crate::view_cmds::note_rects(plan_rect, d3_rect);
        if plan_rect.is_positive() {
            crate::links_ui::paint_plan(self, ctx, plan_rect, t);
            self.welcome(ctx, plan_rect, fire);
        }
        // overlays (draft ghost, tool bar, inspector) belong over the preview, not the column
        match ai_lay {
            Some(lay) if lay.view.is_positive() => lay.view,
            Some(lay) => lay.work,
            None => area,
        }
    }

    /// Floating surfaces over the view (layouts 2 and 4): tool options bar, inspector window
    /// while something is selected, command hint.
    pub(super) fn panel_overlays(
        &mut self,
        ctx: &egui::Context,
        ws: &Workspace,
        area: egui::Rect,
        t: &Tokens,
        fire: &mut Vec<String>,
    ) {
        if !matches!(ws.surface, Surface::ContextBar | Surface::Radial) {
            return;
        }
        let th = &self.sh.theme;
        let frame = egui::Frame::new()
            .fill(th.c("bg.elevated"))
            .stroke(egui::Stroke::new(1.0, th.c("border.overlay")))
            .corner_radius(th.radius("lg"))
            .shadow(th.shadow(2))
            .inner_margin(egui::Margin::symmetric(10, 6));
        egui::Area::new(PanelId::ContextBar.egui_id().into())
            .order(egui::Order::Middle)
            .pivot(egui::Align2::CENTER_BOTTOM)
            .fixed_pos(area.center_bottom() - egui::vec2(0.0, 16.0))
            .constrain_to(area)
            .show(ctx, |ui| {
                frame.show(ui, |ui| {
                    ui.set_min_height(28.0);
                    ui.horizontal_centered(|ui| self.tool_bar(ui, t, fire));
                });
            });
        let inspector_hidden = !ws
            .docks
            .iter()
            .any(|d| d.panel == PanelId::Inspector && d.side != workspace::Side::Hidden);
        if inspector_hidden && !self.ed.selection.is_empty() {
            let w = self.prefs.props_width.min(area.width() * 0.4);
            ddc_ui::z::window(ctx, PanelId::Inspector.egui_id().into());
            egui::Window::new(crate::tr("type_dialog.panels.properties", &[]))
                .id(PanelId::Inspector.egui_id().into())
                .fade_in(false)
                .title_bar(false)
                .resizable(false)
                .fixed_size(egui::vec2(w, (area.height() - 120.0).max(160.0)))
                .pivot(egui::Align2::RIGHT_TOP)
                .default_pos(area.right_top() + egui::vec2(-12.0, 12.0))
                .constrain_to(area)
                .frame(frame.inner_margin(egui::Margin::symmetric(12, 10)))
                .show(ctx, |ui| {
                    ui_kit::with_hint_strip(ui, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| self.props(ui, t));
                    });
                });
        }
        if ws.surface == Surface::Radial {
            let th = &self.sh.theme;
            egui::Area::new(PanelId::Radial.egui_id().into())
                .order(egui::Order::Middle)
                .pivot(egui::Align2::CENTER_TOP)
                .fixed_pos(area.center_top() + egui::vec2(0.0, 12.0))
                .interactable(false)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(crate::tr("type_dialog.panels.radial_hint", &[]))
                            .font(th.font(Role::Caption))
                            .color(th.c("text.secondary")),
                    );
                });
        }
    }

    /// Settings -> "Interface": five live cards; returns the picked layout id.
    pub(super) fn interface_page(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let th = &self.sh.theme;
        let sec = th.c("text.secondary");
        ui.label(RichText::new(crate::tr("dlg.set.iface_lead", &[])).color(sec));
        ui.add_space(8.0);
        let name_of = |w: &Workspace| self.layout_name(w);
        // as many columns as the body holds (three at most): a narrow window gets more rows
        let gap = ui.spacing().item_spacing.x;
        let card = widgets::layout_cards::CARD.x + gap;
        let cols = ((ui.available_width() + gap) / card).floor().clamp(1.0, 3.0) as usize;
        let pick = widgets::layout_cards::grid(ui, th, &self.layouts, &self.layout, cols, &name_of);
        ui.label(RichText::new(crate::tr("dlg.set.iface_keys", &[])).color(sec));
        pick
    }

    /// First run has one layer, the welcome card: the layout chooser is only shown on demand
    /// (`--first-run`); a normal first launch settles it with a toast pointing at the setting.
    pub(super) fn first_run_frame(&mut self, ctx: &egui::Context) {
        if self.force_chooser {
            self.first_run_chooser(ctx);
        } else if self.prefs.path.is_some() && !self.prefs.first_run_done {
            // the setup wizard is the first-start layer; the layout card stays on demand
            self.prefs.first_run_done = true;
            self.prefs.save();
            self.onboarding_first_start();
        } else {
            self.onboarding_first_start();
        }
    }

    /// First run: layout cards in a dialog; only on demand.
    pub(crate) fn first_run_chooser(&mut self, ctx: &egui::Context) {
        let mut pick = None;
        let mut done = false;
        let _ = crate::ui_kit::modal_dialog("first_run", ctx, |ui| {
            let th = &self.sh.theme;
            let c = &self.sh.cat;
            crate::ui_kit::modal_title(ui, c.t("dlg.first.title"));
            ui.label(RichText::new(c.t("dlg.first.lead")).color(th.c("text.secondary")));
            ui.add_space(12.0);
            let name_of = |w: &Workspace| self.layout_name(w);
            pick = widgets::layout_cards::grid(ui, th, &self.layouts, &self.layout, 3, &name_of);
            crate::ui_kit::dialog_footer(ui, |ui| {
                done = crate::ui_kit::primary_action(ui, c.t("dlg.first.start"), None);
            });
        });
        if let Some(id) = pick {
            self.set_layout(&id);
        }
        if done {
            self.force_chooser = false;
            self.prefs.first_run_done = true;
            self.prefs.save();
        }
    }
}

const TOOLS: [&str; 5] = [
    "arch.wall",
    "arch.wall_room",
    "arch.curtain_wall",
    "arch.door",
    "arch.window",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Plan,
    D3,
    View(i64),
    Close(i64),
    /// Context-menu close of several view tabs; the anchor is the tab the menu was opened on.
    CloseSet(tab_strip::CloseKind, Option<i64>),
    /// One of the AI tabs at the end of the bar.
    Ai(AiMode),
}

/// Tab strip over the view area: plan, 3D and open sections/elevations. Tabs that do not fit go
/// to an overflow menu, long names are elided with a tooltip, a context menu closes tabs.
/// Returns the remaining rectangle and the requested action.
fn view_tabs(
    ui: &mut egui::Ui,
    t: &Tokens,
    cat: &ddc_ui::Catalog,
    plan: &str,
    views: &[(i64, String)],
    cur: Tab,
    ai: Option<AiMode>,
) -> (egui::Rect, Option<Tab>) {
    use ddc_ui::sem::{control, icon_size, radius, space, stroke};
    use ddc_ui::widgets::{menu_divider, menu_item_icon, paint_state, rich_tooltip, MenuTone};
    use tab_strip as ts;
    let th = ddc_ui::theme::of(ui.ctx());
    let h = control::TAB_H + space::XS;
    let full = ui.max_rect();
    let strip = egui::Rect::from_min_size(full.min, egui::vec2(full.width(), h));
    let p = ui.painter_at(strip);
    p.rect_filled(strip, 0.0, t.panel);
    p.hline(
        strip.x_range(),
        strip.bottom() - stroke::HAIRLINE / 2.0,
        egui::Stroke::new(stroke::HAIRLINE, t.border),
    );
    // one weight for every tab: the active one is marked by fill and colour, so nothing shifts
    let font = ddc_ui::typo::font(ddc_ui::theme::Role::Tab);
    let name_max = space::XXXL * 3.0;
    let close_w = th.d().control_sm + space::XS;
    let gap = space::XXS;
    let more_w = control::HIT_MIN;
    let left = strip.left() + space::SM;
    // the AI tabs close the bar: the view tabs fit in what they leave
    let right = strip.right() - space::SM - ai_modes::tabs_width(ui, cat);
    let order: Vec<i64> = views.iter().map(|v| v.0).collect();
    let items: Vec<(Tab, String)> = [(Tab::Plan, plan.to_string()), (Tab::D3, cat.t("tabs.view3d"))]
        .into_iter()
        .chain(views.iter().map(|(id, n)| (Tab::View(*id), n.clone())))
        .collect();
    let natural: Vec<f32> = items
        .iter()
        .map(|(_, n)| p.layout_no_wrap(n.clone(), font.clone(), t.text).size().x)
        .collect();
    let widths: Vec<f32> = items
        .iter()
        .zip(&natural)
        .map(|((tab, _), nat)| {
            let cw = if matches!(tab, Tab::View(_)) { close_w } else { 0.0 };
            ts::tab_width(ts::name_width(*nat, name_max), space::MD, cw)
        })
        .collect();
    let active = items.iter().position(|(tab, _)| *tab == cur).unwrap_or(0);
    let fit = ts::tab_fit(&widths, gap, right - left, more_w, active);
    let limit = if fit.hidden.is_empty() { right } else { right - more_w - gap };
    let mut x = left;
    let mut out = None;
    for &i in &fit.shown {
        let (tab, name) = &items[i];
        let tab = *tab;
        let on = tab == cur && ai.is_none();
        let r = egui::Rect::from_min_max(
            egui::pos2(x, strip.top()),
            egui::pos2((x + widths[i]).min(limit), strip.top() + h),
        );
        x += widths[i] + gap;
        let closable = matches!(tab, Tab::View(_));
        let text_max = (r.width() - 2.0 * space::MD - if closable { close_w } else { 0.0 }).max(1.0);
        let id = ui.id().with(("view-tab", format!("{tab:?}")));
        let resp = ui.interact(r, id, egui::Sense::click());
        let hovered = ui.rect_contains_pointer(r);
        let inset = egui::vec2(0.0, space::XS);
        if on {
            p.rect_filled(r.shrink2(inset), radius::MD, t.bg);
            p.hline(
                r.shrink2(egui::vec2(space::SM, 0.0)).x_range(),
                r.bottom() - stroke::INDICATOR * 0.75,
                egui::Stroke::new(stroke::INDICATOR, t.accent),
            );
        } else if hovered {
            p.rect_filled(r.shrink2(inset), radius::MD, t.bg.gamma_multiply(0.6));
        }
        let g = ddc_ui::widgets::elide(ui, name, font.clone(), if on { t.text } else { t.text_dim }, text_max);
        let gy = r.center().y - g.size().y / 2.0;
        p.galley(egui::pos2(r.left() + space::MD, gy), g, t.text);
        let mut closed = false;
        if let Tab::View(vid) = tab {
            if on || hovered {
                let side = th.d().control_sm;
                let cr = egui::Rect::from_center_size(
                    egui::pos2(r.right() - space::SM - side / 2.0, r.center().y),
                    egui::Vec2::splat(side),
                );
                let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(cr));
                if ddc_ui::widgets::close_button(&mut cui, &cat.t("tabs.close")).clicked() {
                    out = Some(Tab::Close(vid));
                    closed = true;
                }
            }
            if resp.middle_clicked() {
                out = Some(Tab::Close(vid));
                closed = true;
            }
        }
        if resp.clicked() && !closed {
            out = Some(tab);
        }
        let anchor = if let Tab::View(v) = tab { Some(v) } else { None };
        resp.context_menu(|ui| {
            for (kind, key) in [
                (ts::CloseKind::This, "tabs.close"),
                (ts::CloseKind::Others, "tabs.close_others"),
                (ts::CloseKind::Right, "tabs.close_right"),
            ] {
                let enabled = !ts::close_set(kind, &order, anchor).is_empty();
                let icon = crate::ctx_menu::verb_icon("close");
                if menu_item_icon(ui, &th, icon, &cat.t(key), "", MenuTone::Normal, enabled).clicked() {
                    out = Some(Tab::CloseSet(kind, anchor));
                    ui.close();
                }
                if kind == ts::CloseKind::This {
                    menu_divider(ui, &th);
                }
            }
        });
        if ts::is_elided(natural[i], text_max) {
            rich_tooltip(resp, &th, name, "", "");
        }
    }
    if !fit.hidden.is_empty() {
        let mr = egui::Rect::from_min_size(
            egui::pos2(right - more_w, strip.top() + space::XS),
            egui::vec2(more_w, h - 2.0 * space::XS),
        );
        let resp = ui.interact(mr, ui.id().with("view-tab-more"), egui::Sense::click());
        let fg = paint_state(ui, &th, mr, &resp, false);
        let c = egui::pos2(
            mr.center().x.round(),
            ddc_ui::px::snap(mr.center().y, ui.ctx().pixels_per_point()),
        );
        let k = icon_size::XS / 4.0;
        p.add(egui::Shape::line(
            vec![
                c + egui::vec2(-k, -k / 2.0),
                c + egui::vec2(0.0, k / 2.0),
                c + egui::vec2(k, -k / 2.0),
            ],
            egui::Stroke::new(stroke::HAIRLINE * 1.5, fg),
        ));
        let resp = rich_tooltip(resp, &th, &cat.t("tabs.more"), "", "");
        egui::Popup::menu(&resp).show(|ui| {
            for &i in &fit.hidden {
                let (tab, name) = &items[i];
                let label = ts::menu_name(name, 28);
                let label = if *tab == cur { format!("\u{2022} {label}") } else { format!("   {label}") };
                if menu_item_icon(ui, &th, None, &label, "", MenuTone::Normal, true).clicked() {
                    out = Some(*tab);
                    ui.close();
                }
            }
        });
    }
    let start = if fit.hidden.is_empty() { x - gap } else { right };
    if let Some(m) = ai_modes::paint_tabs(ui, cat, strip, h, start, ai) {
        out = Some(Tab::Ai(m));
    }
    let rest = egui::Rect::from_min_max(egui::pos2(full.left(), strip.bottom()), full.max);
    (rest, out)
}

/// Context menu of a level (plan row or level row).
fn level_menu(
    r: &egui::Response,
    id: i64,
    act: &mut Option<(i64, u8)>,
    c: &ddc_ui::Catalog,
    th: &ddc_ui::Theme,
) {
    use crate::ctx_menu;
    r.context_menu(|ui| {
        for (k, icon, key, cmd) in [
            (0, Some("open"), "browser.level.open", ctx_menu::OPEN),
            (5, None, "browser.rename", ctx_menu::RENAME),
            (1, Some("level-edit"), "browser.level.edit", ""),
            (3, Some("level"), "browser.level.new", ""),
        ] {
            if ctx_menu::item_icon(ui, th, icon, &c.t(key), cmd).clicked() {
                *act = Some((id, k));
                ui.close();
            }
        }
        ddc_ui::widgets::menu_divider(ui, th);
        if ctx_menu::item_icon(ui, th, Some("delete"), &c.t("browser.delete"), ctx_menu::DELETE).clicked() {
            *act = Some((id, 2));
            ui.close();
        }
    });
}

/// Open the type dialog for `id`.
pub(crate) fn with_type_dialog(ed: &Editor, fam: type_dialog::Fam, id: i64) {
    if fam == type_dialog::Fam::Wall {
        wall_type_editor::open(&ed.model, id, ed.tool.units);
        return;
    }
    let mut d = type_dialog::TypeDraft::load(&ed.model, fam, id, ed.tool.units);
    d.structure = fam.layered();
    browser::with(|s| type_dialog::open_in(&mut s.type_dlg, d, &ed.model));
}

/// Value that differs across the selection (Russian variant).
#[cfg(test)]
const VARIOUS: &str = "<\u{440}\u{430}\u{437}\u{43d}\u{44b}\u{435}>";

/// Common value (`Some(None)` means they differ; `None` means empty).
pub fn common<T: PartialEq + Clone>(vals: &[T]) -> Option<Option<T>> {
    let first = vals.first()?;
    Some(vals.iter().all(|v| v == first).then(|| first.clone()))
}

fn show_common(v: Option<Option<String>>, ru: bool) -> String {
    match v {
        None => "—".into(),
        Some(None) => crate::tr_str(ru, "type_dialog.panels.varies").into(),
        Some(Some(s)) => s,
    }
}

/// Selection categories: (key, label, count), in order of first appearance.
pub fn sel_categories(sel: &[Sel], ru: bool) -> Vec<(&'static str, String, usize)> {
    let mut out: Vec<(&'static str, String, usize)> = vec![];
    for s in sel {
        let stair = match s {
            Sel::Ext(c, _) => match crate::editor::structx::kind_of(*c) {
                Some("stair") => Some("stair"),
                Some("railing") => Some("railing"),
                Some("shaft") => Some("shaft"),
                Some("grid") => Some("grid"),
                Some("ref_plane") => Some("ref_plane"),
                _ => None,
            },
            _ => None,
        };
        if let Some(k) = stair {
            // names come from the locale, not from this table
            match out.iter_mut().find(|c| c.0 == k) {
                Some(c) => c.2 += 1,
                None => out.push((k, crate::tr(&format!("selcat.{k}"), &[]), 1)),
            }
            continue;
        }
        let (k, key) = match s {
            Sel::Wall(_) => ("wall", "type_dialog.selcat.wall"),
            Sel::Opening(_) => ("opening", "type_dialog.selcat.opening"),
            Sel::Curtain(_) | Sel::Panel(..) => ("curtain", "type_dialog.selcat.curtain"),
            Sel::Slab(_) => ("floor", "type_dialog.selcat.floor"),
            Sel::Room(_) => ("room", "type_dialog.selcat.room"),
            Sel::Mass(_) => ("mass", "type_dialog.selcat.mass"),
            Sel::Mep(_) | Sel::Ext(crate::scene_ids::EXT_FITTING, _) => {
                ("mep", "type_dialog.selcat.mep")
            }
            Sel::Comp(_) => ("comp", "type_dialog.selcat.comp"),
            Sel::Ext(c, _) if crate::editor::structx::kind_of(*c) == Some("ramp") => {
                ("ramp", "type_dialog.selcat.ramp")
            }
            Sel::Ext(c, _) if crate::editor::structx::kind_of(*c) == Some("room_sep") => {
                ("room_sep", "type_dialog.selcat.room_sep")
            }
            Sel::Ext(c, _) if crate::editor::structx::kind_of(*c) == Some("zone") => {
                ("zone", "type_dialog.selcat.zone")
            }
            Sel::Anno(_) => ("anno", "type_dialog.selcat.anno"),
            Sel::Ext(..) => (crate::editor::structx::CATEGORY, "type_dialog.selcat.structure"),
            Sel::Body(k, _) => match crate::editor::bodies::category_key(*k) {
                "column" => ("column", "type_dialog.selcat.column"),
                "beam" => ("beam", "type_dialog.selcat.beam"),
                "ceiling" => ("ceiling", "type_dialog.selcat.ceiling"),
                _ => ("imported", "type_dialog.selcat.imported"),
            },
        };
        match out.iter_mut().find(|c| c.0 == k) {
            Some(c) => c.2 += 1,
            None => out.push((k, crate::tr_str(ru, key).into(), 1)),
        }
    }
    out
}

pub(super) fn cat_of(s: &Sel) -> &'static str {
    sel_categories(std::slice::from_ref(s), true)[0].0
}

/// Wall edit from the model as a batch item.
pub fn wall_edit(m: &ddc_model::Model, id: i64) -> Option<ddc_model::Edit> {
    let spec = m
        .sql()
        .query_row(
            "SELECT ax,ay,bx,by,level,base_offset,height,wtype,offset0 FROM wall WHERE id=?1",
            [id],
            |r| {
                Ok(ddc_model::WallSpec {
                    a: (r.get(0)?, r.get(1)?),
                    b: (r.get(2)?, r.get(3)?),
                    level: r.get(4)?,
                    base_offset: r.get(5)?,
                    height: r.get(6)?,
                    wall_type: r.get(7)?,
                    offset0: r.get(8)?,
                })
            },
        )
        .ok()?;
    let bulge = m.wall_bulge(id).ok()?;
    Some(ddc_model::Edit::Wall { id, spec, bulge })
}

/// Type and/or height for all selected walls: one operation, one undo step.
pub fn edit_walls(ed: &mut Editor, ids: &[i64], wtype: Option<i64>, height: Option<f64>) -> bool {
    if height.is_some_and(|h| !(h > 0.0)) {
        ed.status = Some(crate::tr("type_dialog.status.height_positive", &[]));
        return false;
    }
    let mut edits = vec![];
    for &id in ids {
        if let Some(ddc_model::Edit::Wall {
            id,
            mut spec,
            bulge,
        }) = wall_edit(&ed.model, id)
        {
            let before = spec.clone();
            if let Some(t) = wtype {
                spec.wall_type = t;
            }
            if let Some(h) = height {
                spec.height = h;
            }
            if spec != before {
                edits.push(ddc_model::Edit::Wall { id, spec, bulge });
            }
        }
    }
    if edits.is_empty() {
        return false;
    }
    // a lower or shorter wall keeps its openings fitting, or that wall is refused and named
    let (edits, refused) = crate::multi_edit::screen_openings(ed, edits);
    let c = crate::multi_edit::apply(ed, edits, &refused, "type_dialog.status.walls_changed");
    c.is_ok_and(|c| c.changed > 0)
}

/// Sizes for all selected openings (`None` means unchanged): one operation.
/// Each opening is checked as one would be; the refused ones are named, the rest applied.
pub fn edit_openings(ed: &mut Editor, ids: &[i64], dims: [Option<f64>; 3]) -> bool {
    let mut edits = vec![];
    for &id in ids {
        let Some(o) = ed.opening(id).copied() else {
            continue;
        };
        let v = [o.width, o.height, o.sill];
        let n: Vec<f64> = (0..3).map(|k| dims[k].unwrap_or(v[k])).collect();
        if n[..] == v[..] {
            continue;
        }
        edits.push(ddc_model::Edit::Opening {
            id,
            wall: o.wall,
            spec: ddc_model::OpeningSpec {
                along: o.along,
                width: n[0],
                height: n[1],
                sill: n[2],
                kind: o.kind.model_name().into(),
            },
        });
    }
    if edits.is_empty() {
        return false;
    }
    let (edits, refused) = crate::multi_edit::screen_openings(ed, edits);
    let c = crate::multi_edit::apply(ed, edits, &refused, "type_dialog.status.openings_changed");
    c.is_ok_and(|c| c.changed > 0)
}

/// How many walls use the type. Cached per model revision: the panel draws every frame.
fn type_affects(ui: &egui::Ui, ed: &Editor, wtype: i64) -> Option<u64> {
    let key = egui::Id::new(("ddc.props.affects", wtype));
    let rk = ed.cache_key();
    if let Some((rev, n)) = ui.data(|d| d.get_temp::<((u64, u64), u64)>(key)) {
        if rev == rk {
            return Some(n);
        }
    }
    let n = crate::type_use::users(&ed.model, "wall_type", wtype)? as u64;
    ui.data_mut(|d| d.insert_temp(key, (rk, n)));
    Some(n)
}

/// Multi-selection properties: category combo, common values or "<different>", edits apply to all at once.
fn props_multi(
    ui: &mut egui::Ui,
    th: &Theme,
    c: &ddc_ui::Catalog,
    ed: &mut Editor,
    toasts: &mut ddc_ui::notify::Toasts,
) {
    let ru = c.lang == Lang::Ru;
    let cats = sel_categories(&ed.selection, ru);
    let want = browser::with(|s| s.multi_cat);
    let key = prop_multi::active(&ed.selection, want, c.lang);
    let cat = cats
        .iter()
        .find(|x| x.0 == key)
        .unwrap_or(&cats[0])
        .clone();
    let chips = prop_multi::chips(&ed.selection, c.lang);
    let act = prop_multi::header(ui, th, c, ed.selection.len(), &cat.1, &chips, cat.0);
    match act {
        Some(prop_multi::Act::Narrow(k)) => {
            let label = chips.iter().find(|x| x.cat == k).map_or("", |x| x.label.as_str());
            if prop_multi::apply_narrow(ed, k) {
                browser::with(|s| s.multi_cat = None);
                toasts.push_cmd(
                    ddc_ui::notify::Level::Info,
                    prop_multi::narrowed_text(c, label, ed.selection.len()),
                    c.t("props.multi.undo"),
                    "sel.prev",
                );
            }
            return;
        }
        Some(prop_multi::Act::View(k)) => {
            browser::with(|s| s.multi_cat = Some(k));
            return;
        }
        Some(prop_multi::Act::Clear) => {
            ed.selection.clear();
            return;
        }
        None => {}
    }
    let ids: Vec<i64> = ed
        .selection
        .iter()
        .filter(|s| cat_of(s) == cat.0)
        .filter_map(|s| match s {
            Sel::Wall(i)
            | Sel::Opening(i)
            | Sel::Slab(i)
            | Sel::Room(i)
            | Sel::Curtain(i)
            | Sel::Mass(i)
            | Sel::Body(_, i) => Some(*i),
            Sel::Panel(..) | Sel::Mep(_) | Sel::Comp(_) | Sel::Ext(..) | Sel::Anno(_) => None,
        })
        .collect();
    let u = ed.tool.units;
    let tip = crate::tr_str(ru, "type_dialog.panels.enter_apply_to_all");
    let various = show_common(Some(None), ru);
    match cat.0 {
        "wall" => {
            let vals = browser::with(|s| {
                s.wall_vals.get((ed.revision, u), &ids, || {
                    prop_rows::WallVals::build(&*ed, &ids, u)
                })
            });
            let ct = vals.common_type;
            let ch = vals.common_height.clone();
            let mut buf = browser::with(|s| match &s.multi_h {
                Some((k, b)) if *k == ids => b.clone(),
                _ => ch.clone().unwrap_or_default(),
            });
            let (mut new_t, mut new_h) = (None, None);
            let mut edit_type = false;
            if ui_kit::heading(ui, crate::tr_str(ru, "type_dialog.panels.instance")) {
                ui_kit::props(ui, "sel.multi", |ui| {
                    ui_kit::kv_widget(ui, crate::tr_str(ru, "type_dialog.panels.type"), |ui| {
                        let rows = type_items::items(&ed.types, u, ru);
                        match TypePick::new("sel.mtype", &rows, ct)
                            .texts(type_items::texts(ru))
                            .width(ui_kit::WIDGET_W)
                            .show(ui, th)
                        {
                            Some(Pick::Id(id)) => new_t = Some(id),
                            Some(Pick::EditType) => edit_type = true,
                            None => {}
                        }
                    });
                    let hl = crate::tr_str(ru, "type_dialog.panels.height");
                    let hint = if ch.is_none() { various.as_str() } else { "" };
                    new_h = ui_kit::kv_length_hint(ui, hl, &mut buf, u, tip, hint);
                    ui_kit::kv_num(
                        ui,
                        crate::tr_str(ru, "type_dialog.panels.length"),
                        show_common(vals.length.clone(), ru),
                    );
                    ui_kit::kv_num(
                        ui,
                        crate::tr_str(ru, "type_dialog.panels.volume_total"),
                        crate::unit_text::vol_ui(vals.volume),
                    );
                });
                // the spread of a mixed value, most common first
                if let Some(s) = &vals.height_spread {
                    ui_kit::hint(ui, format!("{}: {s}", c.t("props.height")));
                }
                if let Some(s) = &vals.length_spread {
                    ui_kit::hint(ui, format!("{}: {s}", c.t("props.length")));
                }
            }
            browser::with(|s| s.multi_h = Some((ids.clone(), buf)));
            let rows: Vec<(&str, String)> = vec![
                (
                    crate::tr_str(ru, "type_dialog.panels.thickness"),
                    show_common(vals.thickness.clone(), ru),
                ),
                (
                    crate::tr_str(ru, "type_dialog.panels.layers"),
                    show_common(vals.layers.clone(), ru),
                ),
            ];
            let affects = ct.and_then(|id| type_affects(ui, ed, id));
            if ui_kit::heading(ui, &c.t("props.type_head")) {
                prop_rows::type_block(ui, th, c, &rows, affects);
            }
            if edit_type {
                let id = ct.or(vals.first_type).unwrap_or(0);
                with_type_dialog(ed, type_dialog::Fam::Wall, id);
            }
            if new_t.is_some() || new_h.is_some() {
                edit_walls(ed, &ids, new_t, new_h);
                browser::with(|s| s.multi_h = None);
            }
        }
        "opening" => {
            let names = [
                crate::tr_str(ru, "type_dialog.panels.width"),
                crate::tr_str(ru, "type_dialog.panels.height"),
                crate::tr_str(ru, "type_dialog.panels.sill_name"),
            ];
            let vals = browser::with(|s| {
                s.opening_vals.get((ed.revision, u), &ids, || {
                    prop_rows::OpeningVals::build(&*ed, &ids, u)
                })
            });
            let mut bufs = browser::with(|s| match &s.multi_o {
                Some((k, b)) if *k == ids => b.clone(),
                _ => vals.common.clone().map(Option::unwrap_or_default),
            });
            let mut new = [None; 3];
            ui_kit::props(ui, "sel.multi_o", |ui| {
                for k in 0..3 {
                    // differing values: a single line with a hint in the empty field
                    let hint = if vals.common[k].is_none() {
                        various.as_str()
                    } else {
                        ""
                    };
                    new[k] = ui_kit::kv_length_hint(ui, names[k], &mut bufs[k], u, tip, hint);
                }
            });
            for k in 0..3 {
                if let Some(spread) = &vals.spread[k] {
                    ui_kit::hint(ui, format!("{}: {spread}", names[k]));
                }
            }
            browser::with(|s| s.multi_o = Some((ids.clone(), bufs)));
            if new.iter().any(Option::is_some) {
                edit_openings(ed, &ids, new);
                browser::with(|s| s.multi_o = None);
            }
        }
        "room" => {}
        _ => {
            let sels: Vec<Sel> = ed
                .selection
                .iter()
                .filter(|s| cat_of(s) == cat.0)
                .copied()
                .collect();
            let panel = sels.iter().copied().find(|s| matches!(s, Sel::Panel(..)));
            let shared = crate::geom_props::show_multi(ui, ed, &sels);
            match panel {
                _ if shared => {}
                Some(first) => {
                    // panels have no shared rows yet: say so and offer the way to one panel
                    ui_kit::hint(ui, crate::tr_str(ru, "lnp.panels_one"));
                    let keep = shell_kit::secondary(ui, th, crate::tr_str(ru, "lnp.keep_first"));
                    if keep.on_hover_text(crate::tr_str(ru, "lnp.keep_first_tip")).clicked() {
                        ed.selection = vec![first];
                    }
                }
                None => ui_kit::hint(ui, crate::tr_str(ru, "type_dialog.panels.pick_one_element_of")),
            }
        }
    }
    let of_cat: Vec<Sel> =
        ed.selection.iter().filter(|s| cat_of(s) == cat.0).copied().collect();
    crate::params_multi::show(ui, ed, ru, &of_cat);
    if cat.0 != "curtain" {
        crate::query::props_bulk(ui, ru, ed, cat.0);
    }
}

/// True when the project already holds elements (drawn or imported): the empty properties
/// panel then leads with the workbook round trip instead of drawing.
fn props_empty_has_model(ed: &Editor) -> bool {
    !ed.bodies.is_empty()
        || ["wall", "opening", "slab", "room", "curtain_wall"].iter().any(|t| {
            ed.model
                .sql()
                .query_row(&format!("SELECT EXISTS(SELECT 1 FROM {t})"), [], |r| r.get::<_, bool>(0))
                .unwrap_or(false)
        })
}

/// As `props_empty_has_model`, but the table queries run only when `ed.revision` differs
/// from the one stored in `cache` (the panel draws every frame).
fn props_empty_has_model_cached(ed: &Editor, cache: &mut Option<((u64, u64), bool)>) -> bool {
    if !ed.bodies.is_empty() {
        return true;
    }
    let key = ed.cache_key();
    if let Some((k, v)) = *cache {
        if k == key {
            return v;
        }
    }
    let v = props_empty_has_model(ed);
    *cache = Some((key, v));
    v
}

fn props_empty_has_model_ui(ui: &egui::Ui, ed: &Editor) -> bool {
    let key = egui::Id::new("ddc.props.has_model");
    let mut cache = ui.data(|d| d.get_temp::<((u64, u64), bool)>(key));
    let v = props_empty_has_model_cached(ed, &mut cache);
    if let Some(c) = cache {
        ui.data_mut(|d| d.insert_temp(key, c));
    }
    v
}

fn sch_level(warn: bool) -> Level {
    if warn {
        Level::Warning
    } else {
        Level::Info
    }
}

#[cfg(test)]
mod lane_c_tests {
    use super::*;
    use crate::wall_tool::Event;
    use ddc_geom::v2;

    fn room() -> Editor {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.set_mode(Mode::Rect);
        ed.handle(Event::Move(v2(0.0, 0.0)));
        ed.handle(Event::Press(v2(0.0, 0.0)));
        ed.handle(Event::Move(v2(6.0, 4.0)));
        ed.handle(Event::Release(v2(6.0, 4.0)));
        ed.set_mode(Mode::Select);
        ed
    }

    #[test]
    fn common_and_various() {
        assert_eq!(common(&[1, 1, 1]), Some(Some(1)));
        assert_eq!(common(&[1, 2]), Some(None));
        assert_eq!(common::<i32>(&[]), None);
        assert_eq!(show_common(Some(None), true), VARIOUS);
        let sel = [Sel::Wall(1), Sel::Room(2), Sel::Wall(3)];
        let c = sel_categories(&sel, true);
        assert_eq!(c[0], ("wall", crate::app2_fx::fx("shell_walls"), 2));
        assert_eq!(c[1].2, 1);
    }

    #[test]
    fn multi_wall_edit_is_one_undo_step() {
        let mut ed = room();
        let ids: Vec<i64> = ed.walls.iter().map(|w| w.id).collect();
        assert_eq!(ids.len(), 4);
        let undo0 = ed.can_undo();
        let t = ed.types[0].id;
        assert!(edit_walls(&mut ed, &ids, Some(t), Some(2.5)));
        assert!(ed
            .walls
            .iter()
            .all(|w| w.wall_type == t && (w.height - 2.5).abs() < 1e-9));
        ed.undo();
        assert!(ed
            .walls
            .iter()
            .all(|w| (w.height - 3.0).abs() < 1e-9 && w.wall_type != t));
        assert_eq!(ed.can_undo(), undo0);
        ed.redo();
        assert!(ed.walls.iter().all(|w| (w.height - 2.5).abs() < 1e-9));
        assert!(!edit_walls(&mut ed, &ids, None, Some(-1.0)));
        assert!(
            !edit_walls(&mut ed, &ids, None, Some(2.5)),
            "no change — no step"
        );
    }

    #[test]
    fn props_empty_offers_wall_on_empty_model() {
        let ed = Editor::new_in_memory().unwrap();
        assert!(!props_empty_has_model(&ed));
    }

    #[test]
    fn props_empty_offers_xlsx_when_model_has_elements() {
        let ed = room();
        assert!(props_empty_has_model(&ed));
    }

    #[test]
    fn props_empty_check_cached_per_revision() {
        let mut ed = room();
        ed.bodies.clear();
        let mut cache = None;
        assert!(props_empty_has_model_cached(&ed, &mut cache));
        assert!(props_empty_has_model_cached(&ed, &mut cache), "served from cache");
        // Rows vanish behind the editor's back with the revision unchanged: the key still moves.
        for t in ["room", "opening", "slab", "curtain_wall", "wall"] {
            let _ = ed.model.sql().execute(&format!("DELETE FROM {t}"), []);
        }
        assert!(!props_empty_has_model(&ed));
        assert!(!props_empty_has_model_cached(&ed, &mut cache), "refreshed after a swap or undo");
        assert_eq!(cache, Some((ed.cache_key(), false)));
        ed.revision += 1;
        cache = Some((ed.cache_key(), true));
        ed.revision += 1;
        assert!(!props_empty_has_model_cached(&ed, &mut cache), "recomputed after a change");
    }

    #[test]
    fn props_empty_texts_exist_in_both_languages() {
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let c = ddc_ui::Catalog::new(lang);
            for k in [
                "empty.props_model.title",
                "empty.props_model.hint",
                "empty.props_model.action",
            ] {
                assert_ne!(c.t_in(lang, k), k, "{k}");
            }
        }
    }
}
