// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Element warnings: icon in properties, marker on the plan, a 4 s toast
//! for new ones, a list window grouped by type with "Show" and CSV export; a blocking
//! model error is a modal window. The "Model" block in properties shows per-level totals.
use super::EditorApp;
use crate::editor::Sel;
use crate::ui_kit as kit;
use crate::ui_kit::{self, Tokens};
use crate::wall_tool::Mode;

use ddc_model::{ElementWarning, LevelTotals, Totals};
use ddc_ui::notify::Level;
use egui::{self, RichText};
use std::collections::{HashMap, HashSet};

/// Warning text for the UI (the model writes it in English).
pub fn warning_text(w: &ElementWarning) -> String {
    if let Some(why) = w.message.strip_prefix("ramp cannot be built: ") {
        return crate::tr("warn.ramp_broken", &[("why", why)]);
    }
    if let Some(room) = w.message.strip_prefix(crate::room_watch::MSG) {
        return crate::tr("room_status.open", &[("room", room)]);
    }
    if w.table == "zone" {
        return crate::tr("warn.zone_empty", &[]);
    }
    match w
        .message
        .strip_prefix("overlaps wall ")
        .and_then(|r| r.split(':').next())
    {
        Some(other) => {
            crate::tr("type_dialog.warn.overlaps_wall", &[("other", other)])
        }
        None => w.message.clone(),
    }
}

/// Warning kind (group key and label `warn.kind.<k>`).
pub fn warning_kind(w: &ElementWarning) -> &'static str {
    let m = w.message.as_str();
    if w.table == "room" {
        "room_open"
    } else if m.starts_with("overlaps wall") {
        "overlap"
    } else if m.contains("system") {
        "system"
    } else if m.contains("dimension") || m.contains("reference") {
        "dim_ref"
    } else if m.contains("duplicate") {
        "duplicate"
    } else if m.contains("stair") {
        "stair_level"
    } else if m.contains("cut") || m.starts_with("outside its") || m.contains("below the base") {
        "cut"
    } else if m.contains("sketch") || m.contains("open loop") {
        "sketch"
    } else {
        "other"
    }
}

/// Number of the second wall in "overlaps wall N: …".
pub fn other_id(w: &ElementWarning) -> Option<i64> {
    w.message
        .strip_prefix("overlaps wall ")
        .and_then(|r| r.split(':').next())
        .and_then(|n| n.trim().parse().ok())
}

/// Symmetric pairs (#1↔#5 and #5↔#1) collapse into one entry; others are unchanged.
pub fn dedupe_pairs(ws: &[ElementWarning]) -> Vec<ElementWarning> {
    let mut seen: HashSet<(&'static str, i64, i64)> = HashSet::new();
    let mut out = Vec::new();
    for w in ws {
        if let Some(o) = other_id(w) {
            if !seen.insert((w.table, w.id.min(o), w.id.max(o))) {
                continue;
            }
        }
        out.push(w.clone());
    }
    out
}

/// Element labels ("Wall · Masonry 200 · #1") for all list rows and their pairs; one SQL query
/// per rebuild, not per frame.
#[derive(Default)]
pub struct Labels {
    key: Option<((u64, u64), bool)>,
    map: HashMap<(&'static str, i64), String>,
}

impl Labels {
    pub fn get(&self, table: &'static str, id: i64) -> String {
        self.map
            .get(&(table, id))
            .cloned()
            .unwrap_or_else(|| format!("{table} · #{id}"))
    }

    /// Rebuilds labels when the revision or language changed.
    pub fn refresh(
        &mut self,
        m: &ddc_model::Model,
        ws: &[ElementWarning],
        rev: (u64, u64),
        cat: &ddc_ui::Catalog,
    ) {
        let key = Some((rev, cat.lang == ddc_ui::Lang::Ru));
        if self.key == key {
            return;
        }
        self.key = key;
        self.map.clear();
        if ws.is_empty() {
            return;
        }
        let mut types: HashMap<i64, String> = HashMap::new();
        if let Ok(mut st) = m
            .sql()
            .prepare("SELECT w.id, t.name FROM wall w JOIN wall_type t ON t.id = w.wtype")
        {
            if let Ok(rows) =
                st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            {
                types.extend(rows.flatten());
            }
        }
        for w in ws {
            for id in [Some(w.id), other_id(w)].into_iter().flatten() {
                let cat_name = match w.table {
                    "wall" | "slab" | "opening" | "curtain_wall" => {
                        cat.t(&format!("warn.cat.{}", w.table))
                    }
                    t => t.to_string(),
                };
                let ty = (w.table == "wall").then(|| types.get(&id)).flatten();
                let s = match ty {
                    Some(t) => format!("{cat_name} · {t} · #{id}"),
                    None => format!("{cat_name} · #{id}"),
                };
                self.map.insert((w.table, id), s);
            }
        }
    }
}

/// Warnings by kind in order of first appearance.
pub fn grouped(ws: &[ElementWarning]) -> Vec<(&'static str, Vec<&ElementWarning>)> {
    let mut g: Vec<(&'static str, Vec<&ElementWarning>)> = Vec::new();
    for w in ws {
        let k = warning_kind(w);
        match g.iter_mut().find(|x| x.0 == k) {
            Some(x) => x.1.push(w),
            None => g.push((k, vec![w])),
        }
    }
    g
}

/// .csv file of the warnings (type, table, id, text) through the shared writer, in the style of
/// the interface language.
pub fn warnings_csv(ws: &[ElementWarning], kind_name: impl Fn(&str) -> String, ru: bool) -> String {
    let head = ["type", "table", "id", "message"].map(String::from);
    let rows = ws
        .iter()
        .map(|w| [kind_name(warning_kind(w)), w.table.to_string(), w.id.to_string(), warning_text(w)]);
    ddc_geom::csv::file(std::iter::once(head).chain(rows), ddc_geom::csv::Style::for_lang(ru))
}

/// Blocking model error: cannot be ignored.
#[derive(Debug, Clone)]
pub struct Blocking {
    pub text: String,
    pub elements: Vec<Sel>,
}

fn add(s: &mut Totals, o: &Totals) {
    s.walls += o.walls;
    s.doors += o.doors;
    s.windows += o.windows;
    s.wall_plan_area += o.wall_plan_area;
    s.wall_volume += o.wall_volume;
    s.slabs += o.slabs;
    s.slab_area += o.slab_area;
    s.slab_volume += o.slab_volume;
    s.rooms += o.rooms;
    s.room_area += o.room_area;
    s.room_volume += o.room_volume;
    s.curtains += o.curtains;
    s.curtain_panel_area += o.curtain_panel_area;
}

/// "⚠" icon and warning texts of the selected element.
pub fn badge(ui: &mut egui::Ui, warnings: &[ElementWarning], table: &str, id: i64) {
    let mine: Vec<&ElementWarning> = warnings
        .iter()
        .filter(|w| w.table == table && w.id == id)
        .collect();
    if mine.is_empty() {
        return;
    }
    let s = ddc_ui::theme::sem(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(s.warning_subtle)
        .stroke(egui::Stroke::new(1.0, s.warning))
        .corner_radius(ddc_ui::theme::radius::MD)
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for w in mine {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("⚠").color(s.warning));
                    ui.label(warning_text(w));
                });
            }
        });
    ui.add_space(4.0);
}

impl EditorApp {
    /// Recompute warnings and totals when the model revision changes.
    pub(super) fn refresh_warnings(&mut self) {
        let key = self.ed.cache_key();
        if self.warnings.0 == key {
            return;
        }
        let first = self.warnings.0 == (u64::MAX, 0);
        let before = self.warnings.1.len();
        let mut ws = self.ed.model.wall_warnings().unwrap_or_default();
        ws.extend(self.ed.model.cut_warnings().unwrap_or_default());
        ws.extend(self.ed.model.ramp_warnings().unwrap_or_default());
        ws.extend(self.ed.model.zone_warnings().unwrap_or_default());
        ws.extend(self.ed.model.bool_warnings().unwrap_or_default());
        ws.extend(self.ed.room_watch.warnings());
        self.warnings = (key, ws);
        self.warn_uniq = dedupe_pairs(&self.warnings.1);
        self.level_totals = self.ed.model.level_totals().unwrap_or_default();
        self.plan.warn_walls = self
            .warnings
            .1
            .iter()
            .filter(|w| w.table == "wall")
            .map(|w| w.id)
            .collect();
        let n = self.warnings.1.len();
        if !first && n > before {
            // non-blocking: toast (4 s) + ⚠N in the status bar
            let w = &self.warnings.1[n - 1];
            let msg = self.sh.cat.tf(
                "warn.toast",
                &[("n", &n.to_string()), ("text", &warning_text(w))],
            );
            self.sh.toasts.push(Level::Warning, msg, None);
        }
    }

    /// Model error that cannot be ignored: a modal window with a choice.
    pub fn block_error(&mut self, text: impl Into<String>, elements: Vec<Sel>) {
        self.blocking = Some(Blocking {
            text: text.into(),
            elements,
        });
    }

    pub(crate) fn blocking_window(&mut self, ctx: &egui::Context) {
        let Some(b) = self.blocking.clone() else {
            return;
        };
        let c = &self.sh.cat;
        let mut act = None;
        let _ = kit::modal_dialog("blocking", ctx, |ui| {
            ui.set_width(440.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("⛔")
                        .size(ddc_ui::theme::icon_size::MD)
                        .color(self.sh.theme.c("error")),
                );
                kit::modal_title(ui, c.t("warn.block.title"));
            });
            ui.add_space(6.0);
            ui.label(&b.text);
            kit::dialog_footer(ui, |ui| {
                let has = !b.elements.is_empty();
                if kit::primary_action(ui, c.t("warn.block.undo"), None) {
                    act = Some(1);
                }
                if kit::danger_button(ui, c.t("warn.block.delete"), has)
                    .on_disabled_hover_text(c.t("dlg.warn.no_elements"))
                    .clicked()
                {
                    act = Some(0);
                }
                if ui
                    .add_enabled_ui(has, |ui| kit::secondary_button(ui, c.t("warn.show")))
                    .inner
                    .on_disabled_hover_text(c.t("dlg.warn.no_elements"))
                    .clicked()
                {
                    act = Some(2);
                }
            });
        });
        match act {
            Some(0) => {
                self.ed.selection = b.elements.clone();
                self.ed.delete_selection();
                self.blocking = None;
            }
            Some(1) => {
                self.ed.undo();
                self.blocking = None;
            }
            Some(2) => {
                self.ed.selection = b.elements.clone();
                self.blocking = None;
            }
            _ => {}
        }
    }

    /// "Show": switch to the element's level, select it and center the view on it.
    pub fn show_warning(&mut self, table: &str, id: i64) {
        if table == "ramp" || table == "zone" {
            let act = super::panels::browser::core_act(&self.ed, table, id);
            super::panels::browser::select_row(&mut self.ed, act);
            self.view3d = false;
            self.section = None;
            return;
        }
        if table != "wall" {
            return;
        }
        let row: Option<(i64, f64, f64, f64, f64)> = self
            .ed
            .model
            .sql()
            .query_row(
                "SELECT level, ax, ay, bx, by FROM wall WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .ok();
        let Some((lv, ax, ay, bx, by)) = row else {
            return;
        };
        self.ed.set_level(lv);
        self.ed.selection = vec![Sel::Wall(id)];
        self.view3d = false;
        self.section = None;
        self.plan.center = ddc_geom::v2((ax + bx) / 2.0, (ay + by) / 2.0);
        let span = ((bx - ax).hypot(by - ay) * 2.0).max(4.0);
        self.plan.scale = (600.0 / span).clamp(2.0, 2000.0);
    }

    fn export_warnings(&mut self) {
        let c = &self.sh.cat;
        let csv = warnings_csv(&self.warnings.1, |k| c.t(&format!("warn.kind.{k}")), crate::ui_is_ru());
        let Some(p) = crate::file_pick::Dlg::new()
            .set_file_name("warnings.csv")
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };
        let msg = match ddc_io::fsutil::replace_durably_bytes(&p, csv.as_bytes()) {
            Ok(()) => (
                Level::Success,
                c.tf("warn.exported", &[("path", &p.display().to_string())]),
            ),
            Err(e) => (Level::Error, e.to_string()),
        };
        self.sh.toasts.push(msg.0, msg.1, None);
    }

    /// "Warnings" window: groups by kind, "Show", "Export…".
    pub(super) fn warnings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.warnings_open;
        let mut show: Option<(String, i64)> = None;
        let mut export = false;
        self.warn_labels.refresh(
            &self.ed.model,
            &self.warnings.1,
            self.ed.cache_key(),
            &self.sh.cat,
        );
        let c = &self.sh.cat;
        let th = &self.sh.theme;
        let uniq = &self.warn_uniq;
        let n = uniq.len();
        kit::Dialog::new(
            "warnings",
            c.tf("warn.title", &[("n", &n.to_string())]),
            kit::DialogSize::Tool,
        )
        .size([460.0, 480.0])
        // over the view, not the Properties panel on the right
        .first_pos(ctx.screen_rect().center() - egui::vec2(230.0, 240.0))
        .open(&mut open)
        .show(ctx, |ui| {
            // no warnings: the empty note says it once; the count line would repeat it
            if n == 0 {
                kit::empty_note(ui, c.t("warn.none"), None);
                return;
            }
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(c.plural("warn.count", n as u64)).color(th.c("text.secondary")),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    export = ddc_ui::widgets::Button::secondary(&c.t("warn.export")).show(ui, th).clicked();
                });
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (k, items) in grouped(uniq) {
                        let head = format!("{} · {}", c.t(&format!("warn.kind.{k}")), items.len());
                        egui::CollapsingHeader::new(RichText::new(head).strong())
                            .id_salt(("warn.kind", k))
                            .default_open(true)
                            .show(ui, |ui| {
                                for w in items {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("⚠").color(th.c("warning")));
                                        let on = self.ed.selection.contains(&Sel::Wall(w.id));
                                        let name = self.warn_labels.get(w.table, w.id);
                                        let nb = if on {
                                            ddc_ui::widgets::Button::secondary(&name)
                                        } else {
                                            ddc_ui::widgets::Button::quiet(&name)
                                        };
                                        let r = nb.show(ui, th);
                                        if ddc_ui::widgets::Button::quiet(&c.t("warn.show")).small().show(ui, th).clicked()
                                            || r.clicked()
                                        {
                                            show = Some((w.table.to_string(), w.id));
                                        }
                                    });
                                    ui.indent(("warn.txt", w.id), |ui| {
                                        ui.label(
                                            RichText::new(warning_text(w))
                                                .color(th.c("text.secondary")),
                                        );
                                        if let Some(o) = other_id(w) {
                                            let nm = self.warn_labels.get(w.table, o);
                                            if ui.link(nm).clicked() {
                                                show = Some((w.table.to_string(), o));
                                            }
                                        }
                                    });
                                }
                            });
                    }
                });
        });
        self.warnings_open = open;
        if let Some((t, id)) = show {
            self.show_warning(&t, id);
        }
        if export {
            self.export_warnings();
        }
    }

    /// "Model" block: "All levels / Current" toggle and totals.
    pub(super) fn model_totals(&mut self, ui: &mut egui::Ui, _t: &Tokens) {
        ui.horizontal(|ui| {
            let items = [
                crate::tr("type_dialog.warn.all_levels", &[]),
                crate::tr("type_dialog.warn.current", &[]),
            ];
            let refs: Vec<&str> = items.iter().map(String::as_str).collect();
            let th = ddc_ui::theme::of(ui.ctx());
            if let Some(i) = ddc_ui::widgets::segmented(ui, &th, &refs, usize::from(!self.totals_all)) {
                self.totals_all = i == 0;
            }
        });
        let cur = self.ed.level;
        let tot: Option<Totals> = if self.totals_all {
            let mut s = Totals::default();
            for l in &self.level_totals {
                add(&mut s, &l.totals);
            }
            (!self.level_totals.is_empty()).then_some(s)
        } else {
            self.level_totals
                .iter()
                .find(|l| l.level == cur)
                .map(|l: &LevelTotals| l.totals.clone())
        };
        let Some(s) = tot else {
            if ui_kit::empty_state(ui, "totals") {
                self.ed.set_mode(Mode::Polyline);
            }
            return;
        };
        let n = self.warnings.1.len();
        let d = crate::unit_text::summary_decimals();
        ui_kit::props(ui, "tot", |ui| {
            if self.totals_all {
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.levels", &[]), self.level_totals.len().to_string());
            }
            ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.walls", &[]), s.walls.to_string());
            ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.wall_area", &[]), crate::unit_text::area_ui_d(s.wall_plan_area, d));
            ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.wall_vol", &[]), crate::unit_text::vol_ui_d(s.wall_volume, d));
            ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.doors_windows", &[]), format!("{} / {}", s.doors, s.windows));
            if s.slabs > 0 {
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.slabs", &[]), s.slabs.to_string());
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.slab_area", &[]), crate::unit_text::area_ui_d(s.slab_area, d));
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.slab_vol", &[]), crate::unit_text::vol_ui_d(s.slab_volume, d));
            }
            if s.rooms > 0 {
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.rooms", &[]), s.rooms.to_string());
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.room_area", &[]), crate::unit_text::area_ui_d(s.room_area, d));
            }
            if s.curtains > 0 {
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.curtains", &[]), s.curtains.to_string());
                ui_kit::kv_num(ui, &crate::tr("type_dialog.warn.panel_area", &[]), crate::unit_text::area_ui_d(s.curtain_panel_area, d));
            }
        });
        if n > 0 && ui.link(crate::tr("type_dialog.warn.warnings_n", &[("n", n.to_string().as_str())])).clicked() {
            self.warnings_open = true;
        }
    }
}

/// One frame of the open warnings window with `n` fixture warnings for the layout contract.
#[cfg(test)]
pub(crate) fn layout_frame(app: &mut EditorApp, ctx: &egui::Context, n: usize) {
    let ru = app.sh.cat.lang == ddc_ui::Lang::Ru;
    let (dup, lost) = if ru {
        (
            "\u{414}\u{443}\u{431}\u{43b}\u{438}\u{440}\u{443}\u{44e}\u{449}\u{430}\u{44f}\
             \u{441}\u{44f} \u{441}\u{442}\u{435}\u{43d}\u{430}",
            "\u{421}\u{441}\u{44b}\u{43b}\u{43a}\u{430} \
             \u{43f}\u{43e}\u{442}\u{435}\u{440}\u{44f}\u{43d}\u{430}",
        )
    } else {
        ("duplicate wall", "reference lost")
    };
    app.warn_uniq = (0..n)
        .map(|i| ddc_model::ElementWarning {
            table: "wall",
            id: 200 + i as i64,
            message: if i % 2 == 0 { dup.into() } else { lost.into() },
        })
        .collect();
    app.warnings_open = true;
    app.warnings_window(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(id: i64, m: &str) -> ElementWarning {
        ElementWarning {
            table: "wall".into(),
            id,
            message: m.into(),
        }
    }

    #[test]
    fn grouping_and_csv() {
        let ws = vec![
            w(1, "overlaps wall 2: x"),
            w(5, "duplicate instance"),
            w(2, "overlaps wall 1: x"),
        ];
        let g = grouped(&ws);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].0, g[0].1.len()), ("overlap", 2));
        let csv = warnings_csv(&ws, |k| format!("K{k}"), true);
        assert!(csv.starts_with('\u{FEFF}'));
        assert!(csv.ends_with("\r\n"));
        let lines: Vec<&str> = csv.trim_start_matches('\u{FEFF}').lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "type;table;id;message");
        assert!(
            lines[1].starts_with(&format!("Koverlap;wall;1;{}", crate::app2_fx::fx("warn_overlaps_wall"))),
            "{}",
            lines[1]
        );
        assert!(warnings_csv(&[w(3, "say \"hi\"")], |k| k.into(), false).contains("\"say \"\"hi\"\"\""));
        let en = warnings_csv(&[w(4, "=cmd()")], |k| k.into(), false);
        assert!(en.starts_with("\u{FEFF}type,table,id,message\r\n"), "{en}");
        assert!(en.contains(",wall,4,'=cmd()\r\n"), "{en}");
    }

    #[test]
    fn warnings_follow_a_write_that_kept_the_revision() {
        let ed = crate::editor::Editor::new_in_memory().unwrap();
        let mut a = EditorApp::new(ed, None, "t".into());
        a.refresh_warnings();
        let first = a.warnings.0;
        assert_eq!(first, a.ed.cache_key());
        a.ed.model.add_level("Behind", 9.0).unwrap();
        a.refresh_warnings();
        assert_ne!(a.warnings.0, first, "a model swap or undo to an equal revision refreshes");
    }

    #[test]
    fn warnings_export_uses_the_shared_writer() {
        let src = include_str!("warn.rs");
        let code = &src[..src.find("#[cfg(test)]").unwrap()];
        assert!(code.contains("ddc_geom::csv::file("));
        assert!(!code.contains("replace('\"', \"\\\"\\\"\")"), "own quoting is back");
    }
}

#[cfg(test)]
#[test]
fn symmetric_overlap_pairs_collapse() {
    let w = |id: i64, o: i64| ElementWarning {
        table: "wall",
        id,
        message: format!("overlaps wall {o}: corner join skipped, butt ends used"),
    };
    let u = dedupe_pairs(&[w(1, 5), w(5, 1), w(2, 3)]);
    assert_eq!(u.iter().map(|x| x.id).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(other_id(&u[0]), Some(5));
}
