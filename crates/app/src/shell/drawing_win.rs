// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! "File → Import → .dxf": placing a drawing on a level (units, origin), the underlay layer
//! panel with tracing, and the "Drawing → BIM" window (recognition, preview, block rules).

use crate::ui_kit as kit;
use std::path::PathBuf;

use ddc_geom::v2;
use ddc_io::drawing_model::{fixture, BlockRule, RuleKind};
use ddc_ui::theme::{self, space};
use ddc_ui::widgets::{self, Button, NumberField, NumberFormat, TextField};
use egui::{self, RichText};

use super::EditorApp;
use crate::drawing::{Class, Origin, Side, Trace};
use crate::status::Severity;

/// Drawing actions return plain text; the known "nothing to act on" answers are refusals.
fn refusal_or_info(m: &str) -> Severity {
    const KEYS: [&str; 4] = [
        "dlg.dw.st.no_drawing",
        "dlg.dw.st.xf_zero",
        "dlg.dw.st.nothing_picked",
        "dlg.dw.st.drawing_gone",
    ];
    if KEYS.iter().any(|k| crate::tr(k, &[]) == m) {
        Severity::Refusal
    } else {
        Severity::Info
    }
}

/// Level for a file: an existing one or a new one above the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LevelPick {
    Existing(i64),
    New,
}

const UNITS: [(&str, Option<f64>); 6] = [
    ("dlg.dw.unit.auto", None),
    ("dlg.dw.unit.mm", Some(0.001)),
    ("dlg.dw.unit.cm", Some(0.01)),
    ("dlg.dw.unit.m", Some(1.0)),
    ("dlg.dw.unit.in", Some(0.0254)),
    ("dlg.dw.unit.ft", Some(0.3048)),
];

pub struct Form {
    pub files: Vec<(PathBuf, LevelPick)>,
    pub unit: usize,
    pub origin: Origin,
    /// Move a far-away drawing to the origin (the shift stays as its placement offset).
    pub near_origin: bool,
    pub dx: f64,
    pub dy: f64,
    /// PDF sheet scale 1:N.
    pub pdf_ratio: f64,
    pub msg: Vec<String>,
}

impl Default for Form {
    fn default() -> Self {
        Form {
            files: vec![],
            unit: 0,
            origin: Origin::AsIs,
            near_origin: true,
            dx: 0.0,
            dy: 0.0,
            pdf_ratio: 100.0,
            msg: vec![],
        }
    }
}

const DW_LEAD_H: f32 = 44.0;
const DW_FILES_H: f32 = 116.0;
const DW_MSG_H: f32 = 44.0;
const DW_LEVEL_W: f32 = 148.0;
const DW_X_W: f32 = 24.0;
const DW_OPTION_W: f32 = 280.0;
const DW_NUM_W: f32 = 96.0;
const DW_RULE_TEXT_W: f32 = 96.0;
const DW_RULE_KIND_W: f32 = 88.0;

/// Extensions of the "Import" window.
pub const IMPORT_EXT: [&str; 14] = [
    "ifc", "ifczip", "dxf", "dwg", "pdf", "rvt", "rfa", "xlsx", "glb", "gltf", "step", "stp", "stl", "obj",
];

impl EditorApp {
    /// Unified import by extension: IFC goes into the model (one operation), drawings become an underlay,
    /// .rvt/.rfa go through the installed module, .xlsx opens an edit preview.
    pub fn import_any(&mut self, path: &std::path::Path) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.io_busy() {
            self.ed.status = Some(crate::tr("job.busy", &[]));
            return;
        }
        self.prefs.touch_recent(&path.to_string_lossy());
        self.prefs.save();
        self.import_path = path.to_string_lossy().to_string();
        use crate::open::gate::Route;
        let route = crate::open::gate::route(&ext);
        if route == Route::Module && crate::open::jobs(self).running() {
            // one job at a time: the window stays up, the file stays in its field
            self.ed.say(Severity::Refusal, crate::tr("job.busy", &[]));
            return;
        }
        match route {
            // same door as Ctrl+I: busy check, worker thread with progress, one summary
            Route::Worker => {
                use crate::open::Outcome;
                let o = crate::open::request(
                    self,
                    vec![path.to_path_buf()],
                    crate::open::decide::Door::Import,
                );
                if matches!(o, Outcome::Started | Outcome::Card) {
                    self.import_flow.outcome = None;
                    self.import_open = false;
                }
            }
            Route::Drawing => {
                self.open_drawing_import(Some(path.to_path_buf()));
                self.import_open = false;
            }
            Route::Table => {
                self.xlsx_open(path, false);
                self.import_open = false;
            }
            Route::Module => {
                let entries = crate::import::entries(&self.bridges.list);
                match crate::import::entry_for(&entries, path) {
                    Some(crate::import::Entry { dir: Some(dir), .. }) => {
                        self.import_job = Some(crate::import::Job::start(
                            dir.clone(),
                            self.bridges.trust.clone(),
                            path.to_path_buf(),
                        ));
                    }
                    Some(e) => {
                        let why = super::import_win::blocked(&path.to_string_lossy(), &entries)
                            .unwrap_or_else(|| e.label.clone());
                        self.import_failed(path, why);
                    }
                    None => {
                        let why = crate::tr("dlg.imp.unsupported", &[("ext", &ext)]);
                        self.import_failed(path, why);
                    }
                }
            }
        }
    }
    /// A module import (.rvt, .rfa) from the Import window is running.
    pub(crate) fn rvt_import_running(&self) -> bool {
        self.import_job.is_some()
    }

    /// The one Import entry (menu, Ctrl+I): the window with the full format list.
    pub(crate) fn open_import_window(&mut self) {
        self.import_open = true;
    }

    /// Open the drawing import window (with the file, if one is already chosen).
    pub fn open_drawing_import(&mut self, path: Option<PathBuf>) {
        if let Some(p) = path {
            let lv = self.ed.level;
            self.drawing_form.files = vec![(p, LevelPick::Existing(lv))];
        }
        self.plan.drawings.import_open = true;
    }

    /// Drawings from the open flow: each on the level its file name carries, else the active one.
    pub fn open_drawing_files(&mut self, paths: Vec<std::path::PathBuf>) {
        let levels = self.ed.levels();
        let active = self.ed.level;
        self.drawing_form.files = paths
            .into_iter()
            .map(|p| {
                let lv = crate::open::batch::level_for_file(&levels, &p).unwrap_or(active);
                (p, LevelPick::Existing(lv))
            })
            .collect();
        self.drawing_form.msg.clear();
        self.plan.drawings.import_open = true;
    }

    /// Levels for `n` files: in elevation order starting from the current one, missing ones are created.
    fn assign_levels(&self, n: usize) -> Vec<LevelPick> {
        let mut lv = self.ed.levels();
        lv.sort_by(|a, b| a.2.total_cmp(&b.2));
        let start = lv.iter().position(|l| l.0 == self.ed.level).unwrap_or(0);
        (0..n)
            .map(|k| {
                lv.get(start + k)
                    .map_or(LevelPick::New, |l| LevelPick::Existing(l.0))
            })
            .collect()
    }

    fn place_files(&mut self) {
        let files = std::mem::take(&mut self.drawing_form.files);
        let (unit, ratio) = (UNITS[self.drawing_form.unit].1, self.drawing_form.pdf_ratio);
        // the files are read on a worker (cancellable); `place_read` places what it brings
        crate::open::build::start_underlay(self, files, unit, ratio);
    }

    /// The read files of [`Self::place_files`], placed on their levels.
    pub(crate) fn place_read(&mut self, read: Vec<crate::open::build::UnderlayRead>) {
        let (origin, extra0, near) = (
            self.drawing_form.origin,
            v2(self.drawing_form.dx, self.drawing_form.dy),
            self.drawing_form.near_origin,
        );
        let mut msg = vec![];
        let mut first_level = None;
        let mut failed = vec![];
        for (path, pick, res) in read {
            let pick = &pick;
            let (u, unit) = match res {
                Ok(d) => d,
                Err(e) => {
                    msg.push(format!(
                        "{}: {e}",
                        kit::file_name_of(&path.to_string_lossy())
                    ));
                    failed.push((path.clone(), *pick));
                    continue;
                }
            };
            let level = match *pick {
                LevelPick::Existing(id) => id,
                LevelPick::New => match add_level_above(&mut self.ed) {
                    Some(id) => id,
                    None => {
                        let name = kit::file_name_of(&path.to_string_lossy());
                        msg.push(crate::tr("dlg.dw.level_failed", &[("name", &name)]));
                        failed.push((path.clone(), *pick));
                        continue;
                    }
                },
            };
            let name = path.file_name().map_or_else(
                || crate::tr("dlg.dw.default_name", &[]),
                |n| n.to_string_lossy().to_string(),
            );
            let extra = crate::drawing::placement_extra(&u, origin, near, extra0);
            let moved = extra - extra0;
            let mut line = self.plan.drawings.add_underlay(
                u,
                unit,
                &name,
                Some(path.clone()),
                level,
                origin,
                extra,
            );
            if moved.len() > 0.0 {
                // the shift stays as the placement offset; tell where the drawing came from
                line += &crate::tr(
                    "dlg.dw.moved_far",
                    &[
                        ("x", &format!("{:.0}", -moved.x)),
                        ("y", &format!("{:.0}", -moved.y)),
                    ],
                );
            }
            msg.push(line);
            self.prefs.touch_recent(&path.to_string_lossy());
            first_level.get_or_insert(level);
        }
        self.prefs.save();
        if let Some(l) = first_level {
            self.ed.set_level(l);
            self.plan.fit(&self.ed);
            self.plan.drawings.layers_open = true;
        }
        if failed.is_empty() {
            if let Some(m) = msg.last() {
                self.ed.say(Severity::Info, m.clone());
            }
        }
        // failed files stay in the form for a retry
        self.drawing_form.files = failed;
        self.drawing_form.msg = msg;
    }

    pub(super) fn drawing_windows(&mut self, ctx: &egui::Context) {
        self.sync_drawing_undo(ctx);
        if self.plan.drawings.import_open {
            self.drawing_import_window(ctx);
        }
        if self.plan.drawings.layers_open {
            self.drawing_layers_window(ctx);
        }
        if self.plan.drawings.recog_open {
            self.drawing_recog_window(ctx);
        }
        self.sync_drawing_undo(ctx);
    }

    /// Before the windows for undo reached elsewhere in the frame, after them for their edits.
    fn sync_drawing_undo(&mut self, ctx: &egui::Context) {
        if self.plan.drawings.sync_undo(&mut self.ed) {
            ctx.request_repaint();
        }
        if crate::view3d::boxdrag::sync_undo(&mut self.view, &mut self.ed) {
            ctx.request_repaint();
        }
    }

    pub(super) fn drawing_import_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let levels = self.ed.levels();
        let has_drawings = !self.plan.drawings.list.is_empty();
        let nf = NumberFormat::for_lang(self.sh.cat.lang);
        let (mut place, mut pick) = (false, false);
        kit::Dialog::new(
            "drawing_import",
            crate::tr("dlg.dw.import_title", &[]),
            kit::DialogSize::Form,
        )
        .open(&mut open)
        .stable()
        .show(ctx, |ui| {
            let th = theme::of(ui.ctx());
            let form = &mut self.drawing_form;
            let exts = drawing_exts()
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(" / ");
            kit::zone(ui, "dw.lead", DW_LEAD_H, |ui| {
                ui.add(
                    egui::Label::new(crate::tr("dlg.dw.import_lead", &[("exts", &exts)])).wrap(),
                );
            });
            kit::fixed_row(ui, "dw.tools", |ui| {
                pick = Button::secondary(&crate::tr("dlg.dw.pick_files", &[]))
                    .show(ui, &th)
                    .clicked();
                let clear = ui
                    .add_enabled_ui(!form.files.is_empty(), |ui| {
                        Button::quiet(&crate::tr("dlg.dw.clear", &[])).show(ui, &th)
                    })
                    .inner
                    .on_disabled_hover_text(crate::tr("dlg.dw.pick_first", &[]));
                if clear.clicked() {
                    form.files.clear();
                    form.msg.clear();
                }
            });
            ui.add_space(space::XS);
            let new_level = crate::tr("dlg.dw.new_level_lc", &[]);
            let mut level_items: Vec<&str> = levels.iter().map(|l| l.1.as_str()).collect();
            level_items.push(&new_level);
            let mut drop = None;
            kit::zone(ui, "dw.files", DW_FILES_H, |ui| {
                if form.files.is_empty() {
                    kit::empty_note(ui, crate::tr("dlg.dw.no_files", &[]), None);
                }
                for (k, (path, lv)) in form.files.iter_mut().enumerate() {
                    kit::fixed_row(ui, &format!("dw.file{k}"), |ui| {
                        let full = path.display().to_string();
                        let name = kit::file_name_of(&full);
                        let gap = 2.0 * ui.spacing().item_spacing.x;
                        let w = ui.available_width() - DW_LEVEL_W - DW_X_W - gap;
                        kit::fixed_cell(ui, w, |ui| {
                            kit::name_with_tip(ui, &name, &full, false);
                        });
                        let mut sel = match lv {
                            LevelPick::New => levels.len(),
                            LevelPick::Existing(id) => levels
                                .iter()
                                .position(|l| l.0 == *id)
                                .unwrap_or(levels.len()),
                        };
                        let r = widgets::dropdown(
                            ui,
                            &th,
                            ("dwg_lv", k),
                            &mut sel,
                            &level_items,
                            Some(DW_LEVEL_W),
                        );
                        if r.changed() {
                            *lv = levels
                                .get(sel)
                                .map_or(LevelPick::New, |l| LevelPick::Existing(l.0));
                        }
                        let tip = crate::tr("dlg.dw.remove_file", &[]);
                        if widgets::close_button(ui, &tip).clicked() {
                            drop = Some(k);
                        }
                    });
                }
            });
            if let Some(k) = drop {
                form.files.remove(k);
            }
            ui.add_space(space::XS);
            let units: Vec<String> = UNITS.iter().map(|u| crate::tr(u.0, &[])).collect();
            let units: Vec<&str> = units.iter().map(String::as_str).collect();
            kit::labeled_row(ui, &crate::tr("dlg.dw.units", &[]), |ui| {
                widgets::dropdown(
                    ui,
                    &th,
                    "dwg_unit",
                    &mut form.unit,
                    &units,
                    Some(DW_OPTION_W),
                );
            });
            let origins: Vec<String> = Origin::ALL.iter().map(|o| o.label()).collect();
            let origins: Vec<&str> = origins.iter().map(String::as_str).collect();
            kit::labeled_row(ui, &crate::tr("dlg.dw.origin", &[]), |ui| {
                let mut sel = Origin::ALL
                    .iter()
                    .position(|o| *o == form.origin)
                    .unwrap_or(0);
                if widgets::dropdown(ui, &th, "dwg_origin", &mut sel, &origins, Some(DW_OPTION_W))
                    .changed()
                {
                    form.origin = Origin::ALL[sel];
                }
            });
            widgets::checkbox(ui, &th, &mut form.near_origin, &crate::tr("dlg.dw.near_origin", &[]));
            let has_pdf = form
                .files
                .iter()
                .any(|(p, _)| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")));
            let pdf_lbl = crate::tr("dlg.dw.pdf_scale", &[]);
            kit::labeled_row_id(ui, &pdf_lbl, |ui, lid| {
                ui.add_enabled_ui(has_pdf, |ui| {
                    NumberField::new("dwg_pdf", &mut form.pdf_ratio, nf)
                        .labelled_by(lid, &pdf_lbl)
                        .range(1.0, 10000.0)
                        .width(DW_NUM_W)
                        .show(ui, &th)
                        .response
                })
                .inner
                .on_disabled_hover_text(crate::tr("dlg.dw.pdf_only", &[]));
            });
            let off_lbl = crate::tr("dlg.dw.offset", &[]);
            kit::labeled_row_id(ui, &off_lbl, |ui, lid| {
                for (salt, axis, v) in
                    [("dwg_dx", "X", &mut form.dx), ("dwg_dy", "Y", &mut form.dy)]
                {
                    ui.label(axis);
                    let name = format!("{off_lbl} {axis}");
                    NumberField::new(salt, v, nf)
                        .labelled_by(lid, &name)
                        .decimals(2)
                        .step(0.1)
                        .width(DW_NUM_W)
                        .show(ui, &th);
                }
            });
            ui.add_space(space::XS);
            kit::zone(ui, "dw.msg", DW_MSG_H, |ui| {
                for m in &form.msg {
                    ui.add(
                        egui::Label::new(RichText::new(m).color(th.sem().text_secondary)).wrap(),
                    );
                }
            });
            kit::dialog_footer(ui, |ui| {
                let n = form.files.len();
                let label = crate::tr("dlg.dw.place", &[("n", &n.to_string())]);
                let why = crate::tr("dlg.dw.pick_first", &[]);
                if kit::primary_action_table(ui, label, (n == 0).then_some(why.as_str())) {
                    place = true;
                }
                let quiet = |ui: &mut egui::Ui, key: &str| {
                    ui.add_enabled_ui(has_drawings, |ui| {
                        Button::quiet(&crate::tr(key, &[])).show(ui, &th)
                    })
                    .inner
                    .on_disabled_hover_text(crate::tr("dlg.dw.import_first", &[]))
                    .clicked()
                };
                if quiet(ui, "dlg.dw.to_bim_btn") {
                    self.plan.drawings.recog_open = true;
                }
                if quiet(ui, "dlg.dw.layers_btn") {
                    self.plan.drawings.layers_open = true;
                }
            });
        });
        if pick {
            if let Some(files) = crate::file_pick::Dlg::new()
                .add_filter(crate::tr("dlg.dw.filter", &[]), drawing_exts())
                .pick_files()
            {
                let picks = self.assign_levels(files.len());
                self.drawing_form.files = files.into_iter().zip(picks).collect();
                self.drawing_form.msg.clear();
            }
        }
        if place {
            self.place_files();
            // the placed drawing is on screen and its layers window is open: the form is done
            if self.drawing_form.files.is_empty() {
                open = false;
            }
        }
        self.plan.drawings.import_open = open;
    }

    fn drawing_layers_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let levels = self.ed.levels();
        let units = self.ed.tool.units;
        let ds = &mut self.plan.drawings;
        let mut changed = false;
        let mut remove = None;
        let mut status: Option<String> = None;
        let mut new_level = false;
        let mut trace_on = false;
        let mut deleted: Option<usize> = None;
        kit::Dialog::new(
            "drawing_layers",
            crate::tr("dlg.dw.layers_title", &[]),
            kit::DialogSize::Form,
        )
        .stable()
        .first_pos(ctx.screen_rect().right_top() + egui::vec2(-500.0, 120.0))
        .open(&mut open)
        .show(ctx, |ui| {
            if ds.list.is_empty() {
                kit::empty_note(
                    ui,
                    crate::tr("dlg.dw.none", &[]),
                    Some(&crate::tr("dlg.dw.none_hint", &[])),
                );
                return;
            }
            let names: Vec<String> = ds
                .list
                .iter()
                .map(|d| {
                    let lv = levels
                        .iter()
                        .find(|l| l.0 == d.level)
                        .map_or("?", |l| l.1.as_str());
                    format!("{} — {lv}", d.title())
                })
                .collect();
            ds.sel = ds.sel.min(ds.list.len() - 1);
            let th = theme::of(ui.ctx());
            let ru = crate::ui_is_ru();
            let item_names: Vec<&str> = names.iter().map(String::as_str).collect();
            let full_w = ui.available_width();
            widgets::dropdown(ui, &th, "dwg_sel", &mut ds.sel, &item_names, Some(full_w));
            let k = ds.sel;
            let d = &mut ds.list[k];
            ui.horizontal(|ui| {
                changed |= widgets::checkbox(ui, &th, &mut d.visible, &crate::tr("dlg.dw.show", &[]))
                    .changed();
                changed |=
                    widgets::checkbox(ui, &th, &mut d.halftone, &crate::tr("dlg.dw.halftone", &[]))
                        .changed();
                if kit::danger_button(ui, crate::tr("dlg.dw.remove", &[]), true).clicked() {
                    remove = Some(k);
                }
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::tr("dlg.dw.level", &[])).color(th.sem().text_secondary));
                let lv_names: Vec<&str> = levels.iter().map(|l| l.1.as_str()).collect();
                let mut at = levels.iter().position(|l| l.0 == d.level).unwrap_or(0);
                if widgets::dropdown(ui, &th, "dwg_dlv", &mut at, &lv_names, Some(DW_OPTION_W / 2.0))
                    .changed()
                {
                    d.level = levels[at].0;
                    changed = true;
                }
                if Button::quiet(&crate::tr("dlg.dw.new_level", &[]))
                    .show(ui, &th)
                    .on_hover_text(crate::tr("dlg.dw.new_level_tip", &[]))
                    .clicked()
                {
                    new_level = true;
                }
            });
            changed |= super::drawing_kit::layer_actions(ui, &th, &mut d.layers, ru);
            ui.add_space(space::XS);
            changed |= super::drawing_kit::layers_table(ui, &th, &mut d.layers, ru);
            ui.add_space(space::XS);
            ui.separator();
            super::drawing_kit::xf_row(ui, &th, &mut ds.xf, units, ru);
            let copy_id = ui.id().with("dwg_xf_copy");
            let mut copy = ui.data(|d| d.get_temp::<bool>(copy_id)).unwrap_or(false);
            ui.horizontal(|ui| {
                let modes = [
                    crate::tr("drawing_kit.mode_move", &[]),
                    crate::tr("drawing_kit.mode_copy", &[]),
                ];
                let modes: Vec<&str> = modes.iter().map(String::as_str).collect();
                if let Some(i) = widgets::segmented(ui, &th, &modes, usize::from(copy)) {
                    copy = i == 1;
                    ui.data_mut(|d| d.insert_temp(copy_id, copy));
                }
                if Button::secondary(&crate::tr("drawing_kit.apply_drawing", &[]))
                    .show(ui, &th)
                    .clicked()
                {
                    status = Some(ds.apply_xf(true, copy));
                }
            });
            if widgets::checkbox(ui, &th, &mut ds.pick_mode, &crate::tr("dlg.dw.pick_mode", &[]))
                .changed()
            {
                if ds.pick_mode {
                    ds.edit.draw = None;
                    ds.rearm_tool();
                } else {
                    ds.pick = None;
                }
            }
            if ds.pick_mode {
                let n = ds.pick.as_ref().map_or(0, |p| p.1.len());
                ui.horizontal(|ui| {
                    ui.label(crate::tr("dlg.dw.picked", &[("n", &n.to_string())]));
                    ui.add_enabled_ui(n > 0, |ui| {
                        if Button::secondary(&crate::tr("drawing_kit.apply_picked", &[]))
                            .show(ui, &th)
                            .clicked()
                        {
                            status = Some(ds.apply_xf(false, copy));
                        }
                    });
                });
                if super::drawing_kit::delete_row(ui, &th, n, ru) {
                    // a done delete gets the toast only (the status would repeat it); a refusal
                    // (drawing gone) is said, never toasted as "Deleted"
                    let m = ds.delete_pick();
                    if refusal_or_info(&m) == Severity::Refusal {
                        status = Some(m);
                    } else {
                        deleted = Some(n);
                    }
                }
            }
            if let Some(s) = ds.edit_ui(ui) {
                status = Some(s);
            }
            if let Some(s) = ds.xform_ui(ui, units) {
                status = Some(s);
            }
            ui.separator();
            let mut on = ds.trace.is_some();
            if widgets::checkbox(ui, &th, &mut on, &crate::tr("dlg.dw.trace", &[])).changed() {
                ds.trace = on.then_some(Trace {
                    side: Side::Cursor,
                    offset: 0.0,
                });
                trace_on = on;
                ds.rearm_tool();
            }
            if let Some(tr) = &mut ds.trace {
                ui.horizontal(|ui| {
                    let sides: Vec<String> = Side::ALL.iter().map(|s| s.label()).collect();
                    let sides: Vec<&str> = sides.iter().map(String::as_str).collect();
                    let mut at = Side::ALL.iter().position(|s| *s == tr.side).unwrap_or(0);
                    if widgets::dropdown(ui, &th, "dwg_side", &mut at, &sides, Some(DW_OPTION_W / 2.0))
                        .changed()
                    {
                        tr.side = Side::ALL[at];
                    }
                    let off = crate::tr("drawing_kit.trace_offset", &[]);
                    let lr = ui.label(RichText::new(&off).color(th.sem().text_secondary));
                    super::drawing_kit::len_field(
                        ui,
                        &th,
                        "dwg_trace_off",
                        (lr.id, &off),
                        &mut tr.offset,
                        units,
                        ru,
                        Some((-1.0, 1.0)),
                    );
                });
            }
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                if Button::primary(&crate::tr("dlg.dw.to_bim_btn", &[]))
                    .show(ui, &th)
                    .clicked()
                {
                    ds.recog_open = true;
                }
                if Button::secondary(&crate::tr("dlg.dw.import_btn", &[]))
                    .show(ui, &th)
                    .clicked()
                {
                    ds.import_open = true;
                }
            });
        });
        if let Some(k) = remove {
            ds.remove(k);
            status = Some(crate::tr("dlg.dw.st.removed", &[]));
            changed = true;
        }
        if changed {
            ds.touch();
        }
        ds.layers_open = open;
        if !open {
            ds.exit_modes();
        }
        if trace_on {
            // tracing builds walls of the current type, so the "Wall" tool is activated
            self.ed.set_mode(crate::wall_tool::Mode::Polyline);
        }
        if new_level {
            if let Some(id) = add_level_above(&mut self.ed) {
                let ds = &mut self.plan.drawings;
                if let Some(d) = ds.list.get_mut(ds.sel) {
                    d.level = id;
                }
                ds.touch();
                self.ed.set_level(id);
            }
        }
        if let Some(n) = deleted {
            // the delete is one undo step; the toast carries the way back
            self.sh.toasts.push_cmd(
                ddc_ui::notify::Level::Info,
                crate::tr("drawing_kit.deleted", &[("n", &n.to_string())]),
                crate::tr("drawing_kit.undo", &[]),
                "edit.undo",
            );
        }
        if let Some(m) = status {
            let sev = refusal_or_info(&m);
            self.ed.say(sev, m);
        }
    }

    fn drawing_recog_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let (mut run, mut accept, mut save) = (false, false, false);
        let ds = &mut self.plan.drawings;
        kit::Dialog::new(
            "drawing_recognize",
            crate::tr("dlg.dw.to_bim", &[]),
            kit::DialogSize::Form,
        )
        .stable()
        .first_pos(ctx.screen_rect().right_top() + egui::vec2(-500.0, 120.0))
        .open(&mut open)
        .show(ctx, |ui| {
            let Some(d) = ds.list.get(ds.sel) else {
                kit::empty_note(ui, crate::tr("dlg.dw.import_first", &[]), None);
                return;
            };
            ui.label(crate::tr("dlg.dw.recog_of", &[("name", &d.name)]));
            let kth = theme::of(ui.ctx());
            let again = ds.recog.is_some();
            let run_key = if again { "dlg.dw.recog_again" } else { "dlg.dw.recog" };
            let run_text = crate::tr(run_key, &[]);
            let run_btn = if again {
                Button::secondary(&run_text)
            } else {
                Button::primary(&run_text)
            };
            if run_btn.show(ui, &kth).clicked() {
                run = true;
            }
            if let Some(rc) = &mut ds.recog {
                ui.separator();
                let nf = NumberFormat::for_lang(if crate::ui_is_ru() {
                    ddc_ui::i18n::Lang::Ru
                } else {
                    ddc_ui::i18n::Lang::En
                });
                let mut shown = 0;
                for c in Class::ALL {
                    let (on, all) = rc.count_on(c);
                    if all == 0 {
                        continue;
                    }
                    shown += 1;
                    ui.horizontal(|ui| {
                        ui.set_min_height(kth.d().row);
                        let mut full = on == all;
                        let mixed = on > 0 && on < all;
                        if widgets::checkbox_mixed(ui, &kth, &mut full, mixed, &c.label()).changed() {
                            rc.set_class(c, full);
                        }
                        let tools = 2.0 * (kth.d().control + 4.0) + ui.spacing().item_spacing.x;
                        let w = (ui.available_width() - tools).max(0.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(w, kth.d().row),
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                let n = widgets::field::format_number(on as f64, 0, nf);
                                let m = widgets::field::format_number(all as f64, 0, nf);
                                let txt = crate::tr("drawing_kit.n_of_m", &[("n", &n), ("m", &m)]);
                                let ink = if on > 0 {
                                    kth.sem().text_primary
                                } else {
                                    kth.sem().text_secondary
                                };
                                ui.label(RichText::new(txt).color(ink))
                            },
                        );
                        if widgets::glyph_button(ui, "check", true, &crate::tr("drawing_kit.accept_class", &[]))
                            .clicked()
                        {
                            rc.set_class(c, true);
                        }
                        if widgets::glyph_button(ui, "close", true, &crate::tr("drawing_kit.reject_class", &[]))
                            .clicked()
                        {
                            rc.set_class(c, false);
                        }
                    });
                }
                if shown == 0 {
                    kit::empty_note(
                        ui,
                        crate::tr("drawing_kit.no_classes", &[]),
                        Some(&crate::tr("drawing_kit.no_classes_hint", &[])),
                    );
                }
                ui.collapsing(crate::tr("dlg.dw.elements", &[]), |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            let (pr, chosen) = (&rc.props, &mut rc.on);
                            let mut row = |ui: &mut egui::Ui, id: u64, text: String, conf: f64| {
                                let mut on = chosen.contains(&id);
                                let label = format!("{text}  ·  {:.0}%", conf * 100.0);
                                if widgets::checkbox(ui, &kth, &mut on, &label).changed()
                                    && !chosen.remove(&id)
                                {
                                    chosen.insert(id);
                                }
                            };
                            for w in pr.walls.iter().take(200) {
                                let len = (w.b.0 - w.a.0).hypot(w.b.1 - w.a.1);
                                row(
                                    ui,
                                    w.id,
                                    crate::tr(
                                        "dlg.dw.row.wall",
                                        &[
                                            ("t", &format!("{:.0}", w.thickness * 1000.0)),
                                            ("l", &format!("{len:.2}")),
                                        ],
                                    ),
                                    w.confidence,
                                );
                            }
                            for o in pr.openings.iter().take(200) {
                                let k = if o.kind == "door" {
                                    "dlg.dw.row.door"
                                } else {
                                    "dlg.dw.row.window"
                                };
                                let fam = if o.family.is_empty() {
                                    String::new()
                                } else {
                                    format!(" «{}»", o.family)
                                };
                                row(
                                    ui,
                                    o.id,
                                    crate::tr(
                                        k,
                                        &[
                                            ("w", &format!("{:.0}", o.width * 1000.0)),
                                            ("fam", &fam),
                                        ],
                                    ),
                                    o.confidence,
                                );
                            }
                            for r in pr.rooms.iter().take(200) {
                                row(
                                    ui,
                                    r.id,
                                    crate::tr(
                                        "dlg.dw.row.room",
                                        &[
                                            ("num", &r.number.to_string()),
                                            ("name", &r.name),
                                            ("a", &format!("{:.1}", r.area)),
                                        ],
                                    ),
                                    r.confidence,
                                );
                            }
                            for g in &pr.grids {
                                let text = crate::tr("dlg.dw.row.grid", &[("name", &g.name)]);
                                row(ui, g.id, text, g.confidence);
                            }
                            for c in &pr.columns {
                                let s = match c.diameter {
                                    Some(d) => crate::tr(
                                        "dlg.dw.row.col_round",
                                        &[("d", &format!("{:.0}", d * 1000.0))],
                                    ),
                                    None => crate::tr(
                                        "dlg.dw.row.col_rect",
                                        &[
                                            ("b", &format!("{:.0}", c.b * 1000.0)),
                                            ("h", &format!("{:.0}", c.h * 1000.0)),
                                        ],
                                    ),
                                };
                                row(ui, c.id, s, c.confidence);
                            }
                        });
                });
                ui.weak(crate::tr("dlg.dw.click_hint", &[]));
            }
            ui.collapsing(crate::tr("dlg.dw.rules", &[]), |ui| {
                let mut del = None;
                let ru = crate::ui_is_ru();
                let nf = super::drawing_kit::number_format(ru);
                let mm = crate::tr_str(ru, "unit_fmt.unit.mm");
                let kinds = [RuleKind::Door, RuleKind::Window, RuleKind::Ignore];
                let kind_names: Vec<String> = kinds.iter().map(|k| kind_label(*k)).collect();
                let kind_names: Vec<&str> = kind_names.iter().map(String::as_str).collect();
                egui::Grid::new("dwg_rules").num_columns(5).show(ui, |ui| {
                    let mask_hdr = crate::tr("dlg.dw.rule.mask", &[]);
                    let mask_id = ui.weak(mask_hdr.as_str()).id;
                    ui.weak(crate::tr("dlg.dw.rule.kind", &[]));
                    let family_hdr = crate::tr("dlg.dw.rule.family", &[]);
                    let family_id = ui.weak(family_hdr.as_str()).id;
                    let width_hdr = crate::tr("dlg.dw.rule.width", &[]);
                    let width_id = ui.weak(width_hdr.as_str()).id;
                    ui.end_row();
                    for (k, r) in ds.rules.0.iter_mut().enumerate() {
                        TextField::new(&mut r.pattern)
                            .id_salt(("ux58b.rule.pattern", k))
                            .labelled_by(mask_id, &mask_hdr)
                            .width(DW_RULE_TEXT_W)
                            .show(ui, &kth);
                        let mut sel = kinds.iter().position(|x| *x == r.kind).unwrap_or(0);
                        if widgets::dropdown(ui, &kth, ("dwg_rk", k), &mut sel, &kind_names, Some(DW_RULE_KIND_W))
                            .changed()
                        {
                            r.kind = kinds[sel];
                        }
                        TextField::new(&mut r.family)
                            .id_salt(("ux58b.rule.family", k))
                            .labelled_by(family_id, &family_hdr)
                            .width(DW_RULE_TEXT_W)
                            .show(ui, &kth);
                        let shown = r.width.map_or(0.0, |v| v * 1000.0);
                        let mut w = shown;
                        NumberField::new(("ux58b.rule.width", k), &mut w, nf)
                            .labelled_by(width_id, &width_hdr)
                            .range(0.0, 6000.0)
                            .step(10.0)
                            .unit(mm)
                            .width(DW_NUM_W)
                            .show(ui, &kth)
                            .response
                            .on_hover_text(crate::tr("dlg.dw.rule.width_tip", &[]));
                        if w != shown {
                            r.width = (w > 0.0).then_some(w / 1000.0);
                        }
                        if widgets::glyph_button(ui, "delete", true, &crate::tr("rebar_list.remove", &[])).clicked() {
                            del = Some(k);
                        }
                        ui.end_row();
                    }
                });
                if let Some(k) = del {
                    ds.rules.0.remove(k);
                }
                ui.horizontal(|ui| {
                    if Button::secondary(&crate::tr("dlg.dw.add", &[])).small().show(ui, &kth).clicked() {
                        ds.rules.0.insert(
                            0,
                            BlockRule {
                                pattern: "*".into(),
                                kind: RuleKind::Door,
                                family: crate::tr("type_dialog.draw.rule_family", &[]),
                                width: None,
                                height: None,
                                sill: None,
                            },
                        );
                    }
                    if Button::secondary(&crate::tr("dlg.dw.save_rules", &[]))
                        .small()
                        .show(ui, &kth)
                        .clicked()
                    {
                        save = true;
                    }
                });
            });
            if let Some(rc) = &ds.recog {
                let n = rc.chosen().len();
                ui.weak(crate::tr("dlg.dw.undo_hint", &[]));
                kit::dialog_footer(ui, |ui| {
                    // Enter stays with the rule fields above
                    let label = crate::tr("dlg.dw.create", &[("n", &n.to_string())]);
                    if kit::primary_button(ui, label, n > 0)
                        .on_disabled_hover_text(crate::tr("dlg.dw.none_chosen", &[]))
                        .clicked()
                    {
                        accept = true;
                    }
                });
            }
        });
        if save {
            match self.plan.drawings.save_rules() {
                Ok(()) => self.ed.say(Severity::Info, crate::tr("dlg.dw.rules_saved", &[])),
                Err(e) => self.ed.say(
                    Severity::Error,
                    crate::tr("dlg.dw.rules_failed", &[("e", &e.to_string())]),
                ),
            }
        }
        if run {
            let m = self.plan.drawings.recognize(&self.ed);
            let sev = refusal_or_info(&m);
            self.ed.say(sev, m);
        }
        if accept {
            match self.plan.drawings.accept(&mut self.ed) {
                Ok(m) => self.ed.say(Severity::Info, m),
                Err(e) => self.ed.say(
                    Severity::Refusal,
                    crate::tr("dlg.dw.accept_failed", &[("e", &e.to_string())]),
                ),
            }
        }
        self.plan.drawings.recog_open = open;
    }
}

/// IFC into the model via `run_op` (undo-step check only; the app goes through `open`): one undo step on the editor stack, a retry re-reads the file.
#[cfg(test)]
pub(crate) fn import_ifc_op(
    ed: &mut crate::editor::Editor,
    path: &std::path::Path,
) -> Result<ddc_io::ifc::ImportReport, String> {
    use std::sync::{Arc, Mutex};
    let report: Arc<Mutex<Option<ddc_io::ifc::ImportReport>>> = Arc::new(Mutex::new(None));
    let (p, out) = (path.to_path_buf(), report.clone());
    let f = crate::editor::OpFn(Arc::new(move |m: &mut ddc_model::Model| {
        let r = ddc_io::ifc::import_ifc_path(m, &p)
            .map_err(|e| ddc_model::Error::Invalid(e.to_string()))?;
        if let Ok(mut g) = out.lock() {
            *g = Some(r);
        }
        Ok(())
    }));
    ed.run_op(f).map_err(|e| e.to_string())?;
    let r = report.lock().ok().and_then(|mut g| g.take());
    r.ok_or_else(|| crate::tr("type_dialog.draw.no_report", &[]))
}

/// New level above the top: step as between the top two (at least 2.5 m), with a non-repeating name.
pub(crate) fn add_level_above(ed: &mut crate::editor::Editor) -> Option<i64> {
    let mut lv = ed.levels();
    lv.sort_by(|a, b| a.2.total_cmp(&b.2));
    let top = lv.last().map_or(0.0, |l| l.2);
    let step = if lv.len() >= 2 {
        top - lv[lv.len() - 2].2
    } else {
        3.0
    };
    let name = unique_level_name(lv.iter().map(|l| l.1.as_str()), lv.len() + 1);
    ed.add_level(&name, top + step.max(2.5))
}

fn unique_level_name<'a>(names: impl Iterator<Item = &'a str> + Clone, start: usize) -> String {
    (start..)
        .map(|n| format!("\u{42d}\u{442}\u{430}\u{436} {n}"))
        .find(|c| !names.clone().any(|n| n == c))
        .unwrap_or_else(|| "\u{42d}\u{442}\u{430}\u{436}".into())
}

/// Drawing extensions this build can read: .dwg only when the `io::dwg` module is present.
pub fn drawing_exts() -> &'static [&'static str] {
    if ddc_io::dxf::DWG_SUPPORTED {
        &["dxf", "dwg", "pdf"]
    } else {
        &["dxf", "pdf"]
    }
}

/// Extensions of the "Import" window, taking the build's capabilities into account.
pub fn import_exts() -> Vec<&'static str> {
    IMPORT_EXT
        .iter()
        .copied()
        .filter(|e| *e != "dwg" || ddc_io::dxf::DWG_SUPPORTED)
        .collect()
}

fn kind_label(k: RuleKind) -> String {
    let key = match k {
        RuleKind::Door => "dlg.dw.kind.door",
        RuleKind::Window => "dlg.dw.kind.window",
        RuleKind::Ignore => "dlg.dw.kind.ignore",
    };
    crate::tr(key, &[])
}

/// Snapshots: a reference plan on two levels; `underlay` shows layers, `preview` shows recognition,
/// `result` shows the created elements.
/// The drawing form for `--import-demo`: empty, with files, with a failed file.
pub(super) fn demo_import_form(app: &mut EditorApp, state: &str) {
    app.plan.drawings.import_open = true;
    if state != "drawing" {
        let lv = LevelPick::Existing(app.ed.level);
        let dir =
            "C:/Projects/Residential complex North Park/Phase 2/Drawings/Issued for coordination";
        let names = [
            "North Park - Building A - Level 01 - General arrangement plan - rev 12 - final.dwg",
            "Level 02.dxf",
            "Level 03.pdf",
            "Level 04.dxf",
            "Level 05.dxf",
            "Level 06.dxf",
        ];
        app.drawing_form.files = names
            .iter()
            .map(|n| (PathBuf::from(dir).join(n), lv))
            .collect();
    }
    if state == "drawing-error" {
        app.drawing_form.msg = vec![crate::tr("dlg.dw.demo_failure", &[])];
    }
}

pub fn demo(app: &mut EditorApp, stage: &str) -> Result<(), String> {
    let l0 = app.ed.level;
    let ds = &mut app.plan.drawings;
    ds.add(
        &fixture::sample_plan(0),
        "\u{41f}\u{43b}\u{430}\u{43d} 1 \u{44d}\u{442}\u{430}\u{436}\u{430}.dxf",
        None,
        l0,
        None,
        Origin::AsIs,
        v2(0.0, 0.0),
    );
    let l1 = app.ed.add_level("\u{42d}\u{442}\u{430}\u{436} 2", 3.0).ok_or("\u{443}\u{440}\u{43e}\u{432}\u{435}\u{43d}\u{44c}")?;
    let ds = &mut app.plan.drawings;
    ds.add(
        &fixture::sample_plan(1),
        "\u{41f}\u{43b}\u{430}\u{43d} 2 \u{44d}\u{442}\u{430}\u{436}\u{430}.dxf",
        None,
        l1,
        None,
        Origin::AlignFirst,
        v2(0.0, 0.0),
    );
    ds.sel = 0;
    // show the colour override and a halftone
    if let Some(l) = ds.list[0].layers.iter_mut().find(|l| l.name == "A-FURN") {
        l.halftone = true;
    }
    if let Some(l) = ds.list[0].layers.iter_mut().find(|l| l.name == "A-AREA") {
        l.visible = false;
    }
    ds.touch();
    app.ed.set_level(l0);
    match stage {
        "underlay" => app.plan.drawings.layers_open = true,
        "preview" => {
            app.plan.drawings.recognize(&app.ed);
            if let Some(rc) = &mut app.plan.drawings.recog {
                // one column is rejected: shown in grey
                if let Some(c) = rc.props.columns.first() {
                    rc.on.remove(&c.id);
                }
            }
            app.plan.drawings.recog_open = true;
        }
        "result" => {
            for k in 0..2 {
                app.plan.drawings.sel = k;
                app.plan.drawings.recognize(&app.ed);
                app.plan.drawings.accept(&mut app.ed)?;
            }
            app.ed.set_level(l0);
        }
        _ => return Err(format!("--dwg-demo: {stage}?")),
    }
    app.ed.status = None;
    app.plan.fit(&app.ed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    fn wall_count(ed: &Editor) -> i64 {
        ed.model
            .sql()
            .query_row("SELECT count(*) FROM wall", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn ifc_import_is_one_undo_step_and_redoes() {
        let mut src = Editor::new_in_memory().unwrap();
        src.commit_walls(vec![
            (v2(0.0, 0.0), v2(5.0, 0.0)),
            (v2(5.0, 0.0), v2(5.0, 4.0)),
        ]);
        let path = std::env::temp_dir().join(format!("ddc_t99_ifc_{}.ifc", std::process::id()));
        ddc_io::ifc::write_ifc_file(&src.model, &path).unwrap();

        let mut ed = Editor::new_in_memory().unwrap();
        ed.commit_walls(vec![(v2(0.0, 10.0), v2(3.0, 10.0))]);
        let before = wall_count(&ed);
        let r = import_ifc_op(&mut ed, &path).unwrap();
        assert!(r.walls >= 2, "{}", r.walls);
        let after = wall_count(&ed);
        assert!(after > before);
        // undo reverts only the import; the wall that existed before the import remains
        ed.undo();
        assert_eq!(wall_count(&ed), before);
        ed.redo();
        assert_eq!(wall_count(&ed), after);
        ed.undo();
        ed.undo();
        assert_eq!(wall_count(&ed), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ifc_import_error_leaves_undo_stack() {
        let mut ed = Editor::new_in_memory().unwrap();
        ed.commit_walls(vec![(v2(0.0, 0.0), v2(3.0, 0.0))]);
        let bad = std::env::temp_dir().join(format!("ddc_t99_bad_{}.ifc", std::process::id()));
        std::fs::write(&bad, "not a step file").unwrap();
        assert!(import_ifc_op(&mut ed, &bad).is_err());
        ed.undo();
        assert_eq!(
            wall_count(&ed),
            0,
            "undo still pops the wall, not a phantom import"
        );
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn new_level_names_do_not_repeat() {
        let fx = crate::app2_fx::fx;
        let names = [fx("level_floor1"), fx("level_floor3"), fx("level_floor2")];
        assert_eq!(unique_level_name(names.iter().map(|s| s.as_str()), 3), fx("level_floor4"));
        assert_eq!(unique_level_name(names.iter().map(|s| s.as_str()), 1), fx("level_floor4"));
        let other = [fx("level_level1")];
        assert_eq!(
            unique_level_name(other.iter().map(|s| s.as_str()), 2),
            fx("level_floor2")
        );
    }

    #[test]
    fn a_delete_of_a_gone_drawing_is_a_refusal_not_a_deleted_toast() {
        let mut ds = crate::drawing::Drawings::default();
        ds.pick = Some((3, ddc_io::drawing_model::Pick::default()));
        let m = ds.delete_pick();
        assert_eq!(refusal_or_info(&m), Severity::Refusal, "{m}");
        assert_eq!(refusal_or_info(&ds.delete_pick()), Severity::Refusal);
    }

    #[test]
    fn undo_and_redo_commands_reach_the_underlay() {
        let mut a = EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into());
        let level = a.ed.level;
        a.plan.drawings.add(
            &ddc_io::drawing_model::fixture::sample_plan(0),
            "p",
            None,
            level,
            None,
            crate::drawing::Origin::AsIs,
            ddc_geom::v2(0.0, 0.0),
        );
        let before = a.plan.drawings.list[0].u.clone();
        a.plan.drawings.xf = (4.0, 0.0, 0.0);
        a.plan.drawings.apply_xf(true, false);
        a.plan.drawings.sync_undo(&mut a.ed);
        let after = a.plan.drawings.list[0].u.clone();
        assert!(a.run("edit.undo"));
        assert_eq!(a.plan.drawings.list[0].u, before);
        assert!(a.run("edit.redo"));
        assert_eq!(a.plan.drawings.list[0].u, after);
    }

    #[test]
    fn drawing_windows_use_no_raw_widgets() {
        let src = include_str!("drawing_win.rs");
        let prod = &src[..src.find("mod tests {").unwrap_or(src.len())];
        for raw in [
            "ui.button(",
            "small_button(",
            "ui.checkbox(",
            ".checkbox(&mut",
            "egui::DragValue",
            "egui::ComboBox",
            "egui::TextEdit",
            "selectable_value",
            "selectable_label",
            "radio_value",
            "ui.add(egui::",
        ] {
            assert!(!prod.contains(raw), "raw widget {raw} in the drawing windows");
        }
    }

    #[test]
    fn rule_width_field_reads_by_the_ui_decimal_mark() {
        // the rules grid field is a kit NumberField in mm: same parser as every number field
        for (ru, txt) in [(false, "1,250"), (true, "1 250"), (true, "1250,0"), (false, "1250.0")] {
            let nf = super::super::drawing_kit::number_format(ru);
            let v = widgets::field::parse_number(txt, nf);
            assert_eq!(v, Some(1250.0), "{txt} ru={ru}");
        }
        assert_eq!(widgets::field::parse_number("1,25", super::super::drawing_kit::number_format(false)), None);
    }

    #[test]
    fn dwg_offered_only_with_reader() {
        let has = import_exts().contains(&"dwg");
        assert_eq!(has, ddc_io::dxf::DWG_SUPPORTED);
        assert_eq!(drawing_exts().contains(&"dwg"), ddc_io::dxf::DWG_SUPPORTED);
        assert!(import_exts().contains(&"dxf") && import_exts().contains(&"ifc"));
    }

    #[test]
    fn dwg_bytes_without_reader_give_clear_error() {
        if ddc_io::dxf::DWG_SUPPORTED {
            return;
        }
        let p = std::env::temp_dir().join(format!("ddc_t99_{}.dwg", std::process::id()));
        std::fs::write(&p, b"AC1032\0\0\0\0\0\0\0\0").unwrap();
        let e = crate::drawing::read_drawing(&p).unwrap_err();
        assert_eq!(e, crate::drawing::dwg_unavailable());
        let _ = std::fs::remove_file(&p);
    }
}
