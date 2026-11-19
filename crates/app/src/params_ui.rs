// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Project parameters: manager window (create, edit, delete, shared-parameters file) and
//! value rows in the selected element's properties. Edits are Ctrl+Z steps.

use crate::editor::{Editor, OpFn};
use crate::phase_ui::sel_ref;
use crate::shell::{prop_head, prop_rows as lockui};
use crate::ui_kit as kit;
use ddc_model::{ParamDef, ParamSpec, ParamType, PARAM_CATEGORIES};
use ddc_ui::theme::{control, space, Role};
use ddc_ui::widgets::{self, Button, Cell, Column, MenuTone, RowState, Table, TextField};
use ddc_ui::Theme;

/// Width of the caption column of the form.
const LABEL_W: f32 = control::DROPDOWN_W * 0.75;
/// Rows of the parameter list that show before it scrolls.
const LIST_ROWS: f32 = 8.0;
/// Category rows of the form that show before the list scrolls inside itself.
const CAT_ROWS: usize = 4;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

mod drop_confirm;

pub fn type_name(t: ParamType, ru: bool) -> &'static str {
    crate::tr_str(
        ru,
        match t {
            ParamType::Text => "sketch_edit.pt_text",
            ParamType::Int => "sketch_edit.pt_int",
            ParamType::Number => "sketch_edit.pt_number",
            ParamType::Length => "sketch_edit.pt_length",
            ParamType::Area => "sketch_edit.pt_area",
            ParamType::Volume => "sketch_edit.pt_volume",
            ParamType::YesNo => "sketch_edit.pt_yesno",
        },
    )
}

pub fn category_name(c: &str, ru: bool) -> &str {
    match c {
        "door" => crate::tr_str(ru, "sketch_edit.cat_door"),
        "window" => crate::tr_str(ru, "sketch_edit.cat_window"),
        k => crate::phase_ui::kind_name(k, ru),
    }
}

fn blank() -> ParamSpec {
    ParamSpec {
        name: String::new(),
        ptype: ParamType::Text,
        per_type: false,
        unit: String::new(),
        group: String::new(),
        guid: None,
        categories: vec!["wall".into()],
    }
}

struct WinState {
    open: bool,
    /// Form: (id of the parameter being edited, or None for a new one; draft).
    form: (Option<i64>, ParamSpec),
    msg: Option<String>,
    /// Success line of the last export with the file it wrote (offers "Open folder").
    exported: Option<(String, std::path::PathBuf)>,
    /// Parameter whose delete awaits the second click.
    armed: Option<i64>,
    /// Row of the list the row actions apply to.
    sel: Option<i64>,
    /// Edit whose Save waits for the "values will be removed" confirm.
    drop: Option<drop_confirm::Pending>,
}

impl Default for WinState {
    fn default() -> Self {
        Self {
            open: false,
            form: (None, blank()),
            msg: None,
            exported: None,
            armed: None,
            sel: None,
            drop: None,
        }
    }
}

thread_local! {
    static WIN: RefCell<WinState> = RefCell::new(WinState::default());
    /// Property fields: (parameter, table, id) → (displayed value, edit).
    static BUF: RefCell<HashMap<(i64, &'static str, i64), (String, String)>> = RefCell::new(HashMap::new());
}

#[cfg(test)]
thread_local! {
    /// Rects drawn in the last frame, by tag (category key or "save"), for the layout tests.
    static SEEN: RefCell<Vec<(String, egui::Rect)>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
fn seen(tag: &str, r: egui::Rect) {
    SEEN.with(|s| s.borrow_mut().push((tag.to_string(), r)));
}

pub fn open_window() {
    WIN.with(|w| w.borrow_mut().open = true);
}

fn run(
    ed: &mut Editor,
    f: impl Fn(&mut ddc_model::Model) -> Result<(), ddc_model::Error> + Send + Sync + 'static,
) -> Result<(), String> {
    ed.run_op(OpFn(Arc::new(f))).map_err(|e| e.to_string())
}

/// Same name in the file and in the model, but another type, level or guid.
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    pub name: String,
    pub file: ParamSpec,
    pub model: ParamSpec,
}

/// Result of a shared-parameter import. `same` includes the renamed ones.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportReport {
    pub added: usize,
    pub same: usize,
    /// (name in the file, name in the model) for a guid that is already present.
    pub renamed: Vec<(String, String)>,
    pub conflicts: Vec<Conflict>,
}

/// Import a shared-parameters file: match by guid, then by name. New ones are added in a
/// single undo step; same-named ones with a different type/level/guid conflict and are not added.
pub fn import_shared(ed: &mut Editor, text: &str) -> Result<ImportReport, String> {
    let specs = ddc_model::parse_shared(text).map_err(|e| e.to_string())?;
    let have = ed.model.params().map_err(|e| e.to_string())?;
    let mut rep = ImportReport::default();
    let mut new: Vec<ParamSpec> = Vec::new();
    for s in specs {
        let by_guid = s
            .guid
            .as_ref()
            .and_then(|g| have.iter().find(|d| d.spec.guid.as_ref() == Some(g)));
        let by_name = have
            .iter()
            .find(|d| d.spec.name.to_lowercase() == s.name.to_lowercase());
        let same_shape = |m: &ParamSpec| m.ptype == s.ptype && m.per_type == s.per_type;
        match (by_guid, by_name) {
            (Some(g), _) if g.spec.name.to_lowercase() != s.name.to_lowercase() => {
                rep.same += 1;
                rep.renamed.push((s.name.clone(), g.spec.name.clone()));
            }
            (Some(g), _) if same_shape(&g.spec) => rep.same += 1,
            (Some(g), _) => rep.conflicts.push(Conflict {
                name: s.name.clone(),
                file: s.clone(),
                model: g.spec.clone(),
            }),
            (None, Some(n)) => {
                let guid_differs = s.guid.is_some() && n.spec.guid.is_some();
                if same_shape(&n.spec) && !guid_differs {
                    rep.same += 1;
                } else {
                    rep.conflicts.push(Conflict {
                        name: s.name.clone(),
                        file: s.clone(),
                        model: n.spec.clone(),
                    });
                }
            }
            (None, None) => new.push(s),
        }
    }
    rep.added = new.len();
    if !new.is_empty() {
        // one model operation: if any definition fails, nothing is added
        run(ed, move |m| m.add_params(&new).map(|_| ()))?;
    }
    Ok(rep)
}

fn spec_brief(s: &ParamSpec, ru: bool) -> String {
    let lvl = crate::tr_str(
        ru,
        if s.per_type {
            "sketch_edit.p_type_2_2"
        } else {
            "sketch_edit.p_instance_2"
        },
    );
    let g = s.guid.as_deref().unwrap_or("-");
    format!("{}, {}, {}", type_name(s.ptype, ru), lvl, g)
}

fn plural(ru: bool, key: &str, n: usize) -> String {
    use ddc_ui::i18n::{Catalog, Lang};
    static RU: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    static EN: std::sync::OnceLock<Catalog> = std::sync::OnceLock::new();
    let c = if ru {
        RU.get_or_init(|| Catalog::new(Lang::Ru))
    } else {
        EN.get_or_init(|| Catalog::new(Lang::En))
    };
    c.plural(key, n as u64)
}

/// Humanized text of a failed file write (never the raw io message).
pub(crate) fn io_text(e: &dyn std::fmt::Display, ru: bool) -> String {
    crate::ui_err::humanize(&e.to_string(), ru)
}

/// Writes the shared-parameters file via temp + rename (a failed write keeps the old file);
/// returns the confirmation line "N parameters exported to <file>".
pub(crate) fn export_shared(
    ed: &mut Editor,
    path: &std::path::Path,
    ru: bool,
) -> Result<String, String> {
    // guid is pinned in the model before writing: a repeated export gives the same guids
    let defs = ed
        .model
        .ensure_param_guids()
        .and_then(|_| ed.model.params())
        .map_err(|e| e.to_string())?;
    // a user export: durable replace, no .bak left beside the user's file
    ddc_io::fsutil::replace_durably_bytes(path, ddc_model::shared_text(&defs).as_bytes()).map_err(
        |e| crate::tr_lang(ru, "params_list.export_failed", &[("why", &io_text(&e, ru))]),
    )?;
    let file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(crate::tr_lang(
        ru,
        "params_list.exported",
        &[
            ("params", &plural(ru, "params_list.n_params", defs.len())),
            ("file", &file),
        ],
    ))
}

/// First click arms the confirm and returns false; a click on the armed one returns true.
pub(crate) fn click_delete(armed: &mut Option<i64>, id: i64) -> bool {
    if *armed == Some(id) {
        true
    } else {
        *armed = Some(id);
        false
    }
}

/// Confirm question of an armed delete, with the value and element counts.
pub(crate) fn confirm_text(ed: &Editor, d: &ParamDef, ru: bool) -> String {
    let (v, e) = ed.model.param_usage(d.id).unwrap_or((0, 0));
    crate::tr_lang(
        ru,
        "params_list.confirm",
        &[
            ("name", &d.spec.name),
            ("values", &plural(ru, "params_list.n_values", v)),
            ("elements", &plural(ru, "params_list.n_elements", e)),
        ],
    )
}

/// Second click: deletes as one undo step and toasts with an Undo action.
pub(crate) fn delete_confirmed(
    ed: &mut Editor,
    toasts: &mut ddc_ui::notify::Toasts,
    ru: bool,
    id: i64,
) -> Result<(), String> {
    let def = ed
        .model
        .params()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|d| d.id == id)
        .ok_or_else(|| "parameter not found".to_string())?;
    let (v, _) = ed.model.param_usage(id).unwrap_or((0, 0));
    let pushes = ed.pushes;
    run(ed, move |m| m.delete_param(id))?;
    let text = crate::tr_lang(
        ru,
        "params_list.deleted",
        &[
            ("name", &def.spec.name),
            ("values", &plural(ru, "params_list.n_values", v)),
        ],
    );
    toasts.push_step(
        ddc_ui::notify::Level::Success,
        text,
        crate::tr_str(ru, "params_list.undo"),
        "edit.undo",
        ed.entry_since(pushes),
    );
    Ok(())
}

/// Dialog text: totals, then one line per renamed and per conflicting parameter.
fn report_text(r: &ImportReport, ru: bool) -> String {
    let mut out = crate::tr_lang(
        ru,
        "sketch_edit.shared_added",
        &[
            ("n", &r.added.to_string()),
            ("k", &r.same.to_string()),
            ("c", &r.conflicts.len().to_string()),
        ],
    );
    for (f, m) in &r.renamed {
        out.push('\n');
        out.push_str(&crate::tr_lang(
            ru,
            "sketch_edit.shared_renamed",
            &[("name", f), ("model", m)],
        ));
    }
    for c in &r.conflicts {
        out.push('\n');
        out.push_str(&crate::tr_lang(
            ru,
            "sketch_edit.shared_conflict",
            &[
                ("name", &c.name),
                ("file", &spec_brief(&c.file, ru)),
                ("model", &spec_brief(&c.model, ru)),
            ],
        ));
    }
    out
}

/// Why Save is off, or None when the draft can be saved.
pub(crate) fn save_block(s: &ParamSpec, ru: bool) -> Option<&'static str> {
    if s.name.trim().is_empty() {
        Some(crate::tr_str(ru, "sketch_edit.p_enter_a_parameter_name"))
    } else if s.categories.is_empty() {
        Some(crate::tr_str(ru, "sketch_edit.p_tick_at_least_one_category"))
    } else {
        None
    }
}

/// Position of a Yes/No value in the three-state pick: yes, no, not set.
pub(crate) fn yn_index(v: &Option<ddc_model::PVal>) -> usize {
    match v {
        Some(ddc_model::PVal::Bool(true)) => 0,
        Some(ddc_model::PVal::Bool(false)) => 1,
        _ => 2,
    }
}

/// Text the value editor commits for a pick position; empty clears the value.
pub(crate) fn yn_commit(idx: usize) -> &'static str {
    match idx {
        0 => "yes",
        1 => "no",
        _ => "",
    }
}

/// One form line: muted caption in the shared label column, then the control. The control gets
/// the caption's id to name itself by.
fn form_row<R>(
    ui: &mut egui::Ui,
    th: &Theme,
    caption: &str,
    add: impl FnOnce(&mut egui::Ui, egui::Id) -> R,
) -> R {
    ui.horizontal(|ui| {
        let lid = ui
            .allocate_ui_with_layout(
                egui::vec2(LABEL_W, th.d().control),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    // the cell keeps its width, so every control starts on one x
                    ui.set_min_width(LABEL_W);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(caption)
                                .font(th.font(Role::Label))
                                .color(th.sem().text_secondary),
                        )
                        .truncate(),
                    )
                    .id
                },
            )
            .inner;
        add(ui, lid)
    })
    .inner
}

fn form(ui: &mut egui::Ui, th: &Theme, s: &mut ParamSpec, ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let cap = tr("sketch_edit.p_name");
    form_row(ui, th, cap, |ui, lid| {
        TextField::new(&mut s.name)
            .id_salt("ddc.pparam.name")
            .labelled_by(lid, cap)
            .show(ui, th);
    });
    let names: Vec<&str> = ParamType::ALL.iter().map(|t| type_name(*t, ru)).collect();
    let mut at = ParamType::ALL.iter().position(|t| *t == s.ptype).unwrap_or(0);
    form_row(ui, th, tr("sketch_edit.p_data_type"), |ui, _| {
        if widgets::dropdown(ui, th, "ddc.pparam.type", &mut at, &names, None).changed() {
            s.ptype = ParamType::ALL[at];
        }
    });
    if s.ptype == ParamType::Number {
        let cap = tr("sketch_edit.p_unit");
        form_row(ui, th, cap, |ui, lid| {
            TextField::new(&mut s.unit)
                .id_salt("ddc.pparam.unit")
                .hint(tr("sketch_edit.p_pcs"))
                .labelled_by(lid, cap)
                .width(control::DROPDOWN_W)
                .show(ui, th);
        });
    }
    let cap = tr("sketch_edit.p_group");
    form_row(ui, th, cap, |ui, lid| {
        TextField::new(&mut s.group)
            .id_salt("ddc.pparam.group")
            .labelled_by(lid, cap)
            .show(ui, th);
    });
    form_row(ui, th, tr("sketch_edit.p_value_on"), |ui, _| {
        let items = [tr("sketch_edit.p_instance"), tr("sketch_edit.p_type")];
        if let Some(i) = widgets::segmented(ui, th, &items, usize::from(s.per_type)) {
            s.per_type = i == 1;
        }
    });
    ui.add_space(space::SM);
    ui.label(
        egui::RichText::new(tr("sketch_edit.p_categories"))
            .font(th.font(Role::Label))
            .color(th.sem().text_secondary),
    );
    let typed: Vec<&str> = ddc_model::PARAM_TYPE_OF.iter().map(|t| t.0).collect();
    // fixed-width cells in rows: as many columns as the width holds, never past the edge
    let cats: Vec<&str> = PARAM_CATEGORIES.iter().map(|t| t.0).collect();
    kit::scroll_list(ui, "param_categories", CAT_ROWS, th.d().control, |ui| {
        #[cfg(test)]
        seen("cat_list", ui.clip_rect());
        let gap = ui.spacing().item_spacing.x;
        let fit = (ui.available_width() + gap) / (control::DROPDOWN_W + gap);
        let cols = (fit.floor() as usize).max(1);
        let cell = ((ui.available_width() - gap * (cols - 1) as f32) / cols as f32)
            .max(control::FIELD_MIN_W);
        for row in cats.chunks(cols) {
            ui.horizontal(|ui| {
                for &c in row {
                    // type parameter: only for categories that have types
                    let ok = !s.per_type || typed.contains(&c);
                    let mut on = s.categories.iter().any(|x| x == c);
                    let r = ui
                        .allocate_ui_with_layout(
                            egui::vec2(cell, th.d().control),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(cell);
                                ui.add_enabled_ui(ok, |ui| {
                                    widgets::checkbox(ui, th, &mut on, category_name(c, ru))
                                })
                                .inner
                            },
                        )
                        .inner;
                    #[cfg(test)]
                    seen(c, r.rect);
                    if r.changed() {
                        if on {
                            s.categories.push(c.to_string());
                        } else {
                            s.categories.retain(|x| x != c);
                        }
                    }
                }
            });
        }
    });
    if s.per_type {
        s.categories.retain(|c| typed.contains(&c.as_str()));
    }
}

fn share_export(ed: &mut Editor, st: &mut WinState, ru: bool) {
    let f = crate::file_pick::Dlg::new()
        .add_filter("DDC shared parameters", &["txt"])
        .set_file_name("shared_parameters.txt")
        .save_file();
    if let Some(p) = f {
        match export_shared(ed, &p, ru) {
            Ok(line) => {
                st.msg = None;
                st.exported = Some((line, p));
            }
            Err(e) => {
                st.msg = Some(e);
                st.exported = None;
            }
        }
    }
}

fn share_import(ed: &mut Editor, st: &mut WinState, ru: bool) {
    if let Some(p) = crate::file_pick::Dlg::new()
        .add_filter("DDC shared parameters", &["txt"])
        .pick_file()
    {
        st.exported = None;
        st.msg = Some(
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|t| import_shared(ed, &t))
            {
                Ok(r) => report_text(&r, ru),
                Err(e) => e,
            },
        );
    }
}

/// "Project Parameters" window; called every frame.
pub fn window(
    ctx: &egui::Context,
    ed: &mut Editor,
    toasts: &mut ddc_ui::notify::Toasts,
    ru: bool,
) {
    let mut st = WIN.with(|w| std::mem::take(&mut *w.borrow_mut()));
    if !st.open {
        WIN.with(|w| *w.borrow_mut() = st);
        return;
    }
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let th = ddc_ui::theme::of(ctx);
    let mut open = true;
    kit::Dialog::new(
        "project_parameters",
        tr("sketch_edit.p_project_parameters"),
        kit::DialogSize::Tool,
    )
    .open(&mut open)
    .stable()
    .show(ctx, |ui| {
        kit::pinned_footer(ui, |ui, footer| {
        if footer {
            footer_row(ui, &th, ed, &mut st, ru);
            return;
        }
        let defs: Vec<ParamDef> = ed.model.params().unwrap_or_default();
        let mut del = None;
        let mut share = None;
        ui.horizontal(|ui| {
            kit::section_title(ui, tr("params_list.list_title"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = Button::secondary(tr("params_list.shared_menu")).show(ui, &th);
                egui::Popup::menu(&r).show(|ui| {
                    let rows = [
                        (tr("sketch_edit.p_export_shared"), true),
                        (tr("sketch_edit.p_import_shared"), false),
                    ];
                    for (label, is_export) in rows {
                        if widgets::menu_item(ui, &th, label, "", MenuTone::Normal, true).clicked()
                        {
                            share = Some(is_export);
                        }
                    }
                });
            });
        });
        if defs.is_empty() {
            kit::empty_note(
                ui,
                tr("sketch_edit.p_no_parameters_yet"),
                Some(tr("sketch_edit.p_fill_in_the_form_below_and_save")),
            );
        } else {
            if st.sel.is_some_and(|s| !defs.iter().any(|d| d.id == s)) {
                st.sel = None;
            }
            let titles = [
                tr("sketch_edit.p_name"),
                tr("sketch_edit.p_type_2"),
                tr("sketch_edit.p_on"),
                tr("sketch_edit.p_categories"),
            ];
            let mid_w = control::DROPDOWN_W * 0.75;
            let cols = [
                Column::text(titles[0]).sortable(false),
                Column::text(titles[1]).sortable(false).fixed(mid_w),
                Column::text(titles[2]).sortable(false).fixed(mid_w),
                Column::text(titles[3]).sortable(false),
            ];
            let cats: Vec<String> = defs
                .iter()
                .map(|d| {
                    let v: Vec<&str> = d
                        .spec
                        .categories
                        .iter()
                        .map(|c| category_name(c, ru))
                        .collect();
                    v.join(", ")
                })
                .collect();
            let mut sort = None;
            let cur = st.sel;
            let out = Table::new("ddc.pparam.list", &cols, defs.len())
                .max_height(th.d().row * LIST_ROWS)
                .show(
                    ui,
                    &th,
                    &mut sort,
                    |r| {
                        if Some(defs[r].id) == cur {
                            RowState::Selected
                        } else {
                            RowState::Normal
                        }
                    },
                    |r, c| match c {
                        0 => Cell::text(defs[r].spec.name.clone()),
                        1 => Cell::text(type_name(defs[r].spec.ptype, ru)),
                        2 => Cell::muted(if defs[r].spec.per_type {
                            tr("sketch_edit.p_type_2_2")
                        } else {
                            tr("sketch_edit.p_instance_2")
                        }),
                        _ => Cell::muted(cats[r].clone()).with_tip(cats[r].clone()),
                    },
                );
            let at = defs.iter().position(|d| Some(d.id) == cur);
            let pick = out.clicked.or(out.double_clicked).or_else(|| {
                out.moved.map(|m| {
                    let to = at.map_or(-1, |a| a as isize) + m;
                    to.clamp(0, defs.len() as isize - 1) as usize
                })
            });
            if let Some(d) = pick.and_then(|i| defs.get(i)) {
                st.sel = Some(d.id);
            }
            let edit_row = out.double_clicked.or(if out.activated { at } else { None });
            if let Some(d) = edit_row.and_then(|i| defs.get(i)) {
                st.form = (Some(d.id), d.spec.clone());
            }
            if let Some(d) = st.sel.and_then(|s| defs.iter().find(|d| d.id == s)) {
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    let ictx = ui.ctx().clone();
                    let (edit, delete) = widgets::with_icons(&ictx, |icons| {
                        let e = widgets::icon_button(
                            ui,
                            icons,
                            &th,
                            "properties",
                            false,
                            tr("sketch_edit.p_edit"),
                        );
                        let x = widgets::icon_button_danger(
                            ui,
                            icons,
                            &th,
                            "delete",
                            tr("params_list.delete_tip"),
                        );
                        (e.clicked(), x.clicked())
                    });
                    if edit {
                        st.form = (Some(d.id), d.spec.clone());
                    }
                    if delete && click_delete(&mut st.armed, d.id) {
                        del = Some(d.id);
                    }
                    kit::elided_weak(ui, &d.spec.name);
                });
            }
        }
        if let Some(id) = st.armed {
            match defs.iter().find(|d| d.id == id) {
                None => st.armed = None,
                Some(d) => {
                    let text = confirm_text(ed, d, ru);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(text);
                        // never the default action: Enter does nothing, only a click deletes
                        if kit::confirm_danger_button(ui, tr("params_list.confirm_btn"), true).clicked() {
                            del = Some(id);
                        }
                        if Button::quiet(tr("params_list.cancel")).show(ui, &th).clicked() {
                            st.armed = None;
                        }
                    });
                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        st.armed = None;
                    }
                }
            }
        }
        if let Some(id) = del {
            st.armed = None;
            st.exported = None;
            st.msg = delete_confirmed(ed, toasts, ru, id).err();
            if st.form.0 == Some(id) {
                st.form = (None, blank());
            }
        }
        match share {
            Some(true) => share_export(ed, &mut st, ru),
            Some(false) => share_import(ed, &mut st, ru),
            None => {}
        }
        ui.add_space(space::SM);
        kit::section_title(
            ui,
            match st.form.0 {
                Some(_) => tr("sketch_edit.p_edit_parameter"),
                None => tr("sketch_edit.p_new_parameter"),
            },
        );
        form(ui, &th, &mut st.form.1, ru);
        if let Some((line, file)) = &st.exported {
            ui.horizontal_wrapped(|ui| {
                ui.label(line);
                if Button::quiet(tr("params_list.open_folder")).show(ui, &th).clicked() {
                    crate::save::report::open_folder(file);
                }
            });
        }
        if let Some(m) = &st.msg {
            ui.label(m);
        }
        });
    });
    if let Some(r) = drop_confirm::show(ctx, ed, &mut st.drop, ru) {
        st.msg = r.err();
        if st.msg.is_none() {
            st.form = (None, blank());
        }
    }
    st.open = open;
    WIN.with(|w| *w.borrow_mut() = st);
}

/// Footer row pinned below the scrolling body: Save on the far right, Cancel (when editing)
/// and the reason Save is off to its left, elided before they can push Save out.
fn footer_row(ui: &mut egui::Ui, th: &Theme, ed: &mut Editor, st: &mut WinState, ru: bool) {
    let tr = |k: &'static str| crate::tr_str(ru, k);
    let blocked = save_block(&st.form.1, ru);
    if kit::primary_action(ui, tr("sketch_edit.p_save"), blocked) {
        let mut s = st.form.1.clone();
        s.name = s.name.trim().to_string();
        let r = match st.form.0 {
            Some(id) => drop_confirm::save_edit(ed, &mut st.drop, id, s),
            None => Some(run(ed, move |m| m.add_param(&s).map(|_| ()))),
        };
        if let Some(r) = r {
            st.msg = r.err();
            if st.msg.is_none() {
                st.form = (None, blank());
            }
        }
    }
    #[cfg(test)]
    seen("save", ui.min_rect());
    if st.form.0.is_some() && Button::quiet(tr("sketch_edit.p_cancel")).show(ui, th).clicked() {
        st.form = (None, blank());
    }
    if let Some(why) = blocked {
        ui.add(
            egui::Label::new(egui::RichText::new(why).color(th.sem().text_secondary)).truncate(),
        );
    }
}

pub(crate) fn shown(
    v: &Option<ddc_model::PVal>,
    t: ParamType,
    u: crate::units::Units,
    ru: bool,
) -> String {
    use ddc_model::PVal;
    match v {
        None => String::new(),
        Some(PVal::Bool(b)) => match (b, ru) {
            (true, _) => crate::tr_str(ru, "sketch_edit.yes").into(),
            (false, _) => crate::tr_str(ru, "sketch_edit.no").into(),
        },
        Some(PVal::Num(x)) if t == ParamType::Length => crate::units::edit_length(*x, u),
        Some(p) => p.text(),
    }
}

/// Value of one parameter on the element, the key of its lock, whether it is locked and the
/// value its type holds (for the «differs from type» marker), and who set the value.
type PRow = (
    Option<ddc_model::PVal>,
    Option<lockui::LockKey>,
    bool,
    Option<ddc_model::PVal>,
    Option<ddc_model::ValueSrc>,
);

fn prow(ed: &Editor, d: &ParamDef, kind: &str, id: i64) -> PRow {
    let v = ed.model.param_value_of(d, kind, id).ok().flatten();
    // the lock is on the row that holds the value: the type row for a type parameter
    let lk: Option<lockui::LockKey> = if d.spec.per_type {
        ed.model
            .type_of(kind, id)
            .ok()
            .flatten()
            .map(|(tk, tid)| (tk.to_string(), tid, d.spec.name.clone()))
    } else {
        Some((kind.to_string(), id, d.spec.name.clone()))
    };
    let locked = lk
        .as_ref()
        .is_some_and(|k| ed.model.param_locked(&k.0, k.1, &k.2).unwrap_or(false));
    let src = lk
        .as_ref()
        .and_then(|k| ed.model.value_src(&k.0, k.1, &k.2).ok().flatten());
    (v, lk, locked, prop_head::type_value(ed, d, kind, id), src)
}

/// Selected element properties: values of the project parameters for its category.
pub fn props(ui: &mut egui::Ui, ed: &mut Editor, ru: bool) {
    lockui::lock_hot_clear();
    let Some((kind, id)) = ed.selection.first().and_then(|s| sel_ref(*s)) else {
        return;
    };
    // definitions, values and locks are read once per cache key, not every frame
    let rk = ed.cache_key();
    let ck = egui::Id::new(("ddc.pparam.rows", kind, id));
    let hit = ui
        .data(|d| d.get_temp::<((u64, u64), String, Vec<ParamDef>, Vec<PRow>)>(ck))
        .filter(|c| c.0 == rk);
    let (cat, defs, rows) = match hit {
        Some(c) => (c.1, c.2, c.3),
        None => {
            let (cat, defs) = match ed.model.category_of(kind, id) {
                Ok(Some(cat)) => (cat.to_string(), ed.model.params_for(cat).unwrap_or_default()),
                _ => (String::new(), vec![]),
            };
            let rows: Vec<PRow> = defs.iter().map(|d| prow(ed, d, kind, id)).collect();
            ui.data_mut(|d| d.insert_temp(ck, (rk, cat.clone(), defs.clone(), rows.clone())));
            (cat, defs, rows)
        }
    };
    if defs.is_empty() {
        return;
    }
    let u = ed.tool.units;
    let th = ddc_ui::theme::of(ui.ctx());
    let mut commit = None;
    let mut toggle: Option<lockui::LockKey> = None;
    let mut reset: Option<ParamDef> = None;
    let mut verify: Option<lockui::LockKey> = None;
    if crate::ui_kit::heading_keyed(
        ui,
        &prop_head::section_key(&cat, "sketch_edit.props_title"),
        crate::tr_str(ru, "sketch_edit.props_title"),
        Some(defs.len()),
    ) {
        crate::ui_kit::props(ui, "sel.pparam", |ui| {
            for (d, row) in defs.iter().zip(&rows) {
                let (v, lk, locked) = (&row.0, row.1.clone(), row.2);
                // who set the value, beside the lock; a click on an AI one marks it verified
                let mut src_badge = |ui: &mut egui::Ui| {
                    if let Some(s) = &row.4 {
                        if crate::value_src_ui::badge(ui, s, ru) {
                            verify = row.1.clone();
                        }
                    }
                };
                let ov = v.is_some() && prop_head::overridden(v.as_ref(), row.3.as_ref());
                let cur = shown(v, d.spec.ptype, u, ru);
                // units come from the UI unit system (as in schedules)
                let unit = match d.spec.ptype {
                    ParamType::Length => crate::schedule::Kind::Len.unit(u),
                    ParamType::Area => crate::schedule::Kind::Area.unit(u),
                    ParamType::Volume => crate::schedule::Kind::Vol.unit(u),
                    ParamType::Number => d.spec.unit.as_str(),
                    _ => "",
                };
                let mut key = d.spec.name.clone();
                if d.spec.per_type {
                    key += crate::tr_str(ru, "sketch_edit.per_type_suffix");
                }
                let mut hot = false;
                if locked {
                    let (num, unit_txt) = prop_head::value_cell(&cur, unit);
                    crate::ui_kit::kv_widget(ui, &key, |ui| {
                        let b = lockui::lock_toggle(ui, true);
                        if b.clicked() {
                            toggle = lk.clone();
                        }
                        src_badge(ui);
                        if !unit_txt.is_empty() {
                            ui.label(egui::RichText::new(unit_txt).color(crate::ui_kit::weak(ui)));
                        }
                        let l = ui
                            .label(num)
                            .on_hover_text(crate::tr("locked_values.locked_tip", &[]));
                        hot = b.hovered() || l.hovered();
                    });
                    if let (true, Some(k)) = (hot, &lk) {
                        lockui::lock_hot_set((kind, id), k.clone());
                    }
                    continue;
                }
                if d.spec.ptype == ParamType::YesNo {
                    let names = [
                        crate::tr_str(ru, "sketch_edit.yes"),
                        crate::tr_str(ru, "sketch_edit.no"),
                        crate::tr_str(ru, "params_list.not_set"),
                    ];
                    let mut at = yn_index(v);
                    crate::ui_kit::kv_widget(ui, &key, |ui| {
                        if lk.is_some() {
                            let b = lockui::lock_toggle(ui, false);
                            if b.clicked() {
                                toggle = lk.clone();
                            }
                            hot = b.hovered();
                        }
                        src_badge(ui);
                        if ov && prop_head::override_dot(ui).clicked() {
                            reset = Some(d.clone());
                        }
                        // fills what the lock and override marks leave of the value column
                        let w = ui.available_width().min(crate::ui_kit::WIDGET_W);
                        let c = widgets::dropdown(
                            ui,
                            &th,
                            ("ddc.pparam.yn", d.id),
                            &mut at,
                            &names,
                            Some(w),
                        );
                        if c.changed() {
                            commit = Some((d.id, yn_commit(at).to_string()));
                        }
                        hot |= c.hovered() || c.has_focus();
                    });
                    if let (true, Some(k)) = (hot, &lk) {
                        lockui::lock_hot_set((kind, id), k.clone());
                    }
                    continue;
                }
                let k = (d.id, kind, id);
                let mut buf = BUF.with(|b| {
                    let mut b = b.borrow_mut();
                    let e = b.entry(k).or_insert_with(|| (cur.clone(), cur.clone()));
                    if e.0 != cur {
                        *e = (cur.clone(), cur.clone());
                    }
                    e.1.clone()
                });
                let tip = crate::tr_str(ru, "sketch_edit.value_tip");
                if let Some(k) = &lk {
                    let (te, b, done) = crate::ui_kit::kv_widget(ui, &key, |ui| {
                        let b = lockui::lock_toggle(ui, false);
                        src_badge(ui);
                        if ov && prop_head::override_dot(ui).clicked() {
                            reset = Some(d.clone());
                        }
                        if !unit.is_empty() {
                            ui.label(egui::RichText::new(unit).color(crate::ui_kit::weak(ui)));
                        }
                        let out = TextField::new(&mut buf)
                            .labelled_by(
                                egui::Id::new(("ddc.pparam.lbl", d.id, kind, id)),
                                &key,
                            )
                            .id_salt(("ddc.pparam.value", d.id, kind, id))
                            .right_aligned()
                            .width(ui.available_width())
                            .show(ui, &th);
                        (out.response.on_hover_text(tip), b, out.committed)
                    });
                    if b.clicked() {
                        toggle = Some(k.clone());
                    }
                    if te.hovered() || te.has_focus() || b.hovered() {
                        lockui::lock_hot_set((kind, id), k.clone());
                    }
                    if done {
                        commit = Some((d.id, buf.clone()));
                    }
                } else if let Some(t) = crate::ui_kit::kv_edit(ui, &key, &mut buf, unit, tip) {
                    commit = Some((d.id, t));
                }
                BUF.with(|b| {
                    if let Some(e) = b.borrow_mut().get_mut(&k) {
                        e.1 = buf;
                    }
                });
            }
        });
    }
    if let Some((pid, text)) = commit {
        crate::schedule::extra::edit_param(ed, pid, kind, id, text.trim());
    }
    if let Some(k) = toggle {
        lockui::toggle_lock(ed, &k);
    }
    if let Some(d) = reset {
        prop_head::reset_to_type(ed, &d, kind, id);
    }
    if let Some(k) = verify {
        crate::value_src_ui::verify(ed, &[k]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_is_blocked_without_name_and_says_why() {
        let mut s = blank();
        for ru in [false, true] {
            let why = save_block(&s, ru).expect("empty name blocks Save");
            assert_eq!(why, crate::tr_str(ru, "sketch_edit.p_enter_a_parameter_name"));
            assert_ne!(why, "sketch_edit.p_enter_a_parameter_name");
        }
        s.name = "   ".into();
        assert!(save_block(&s, false).is_some());
        s.name = "Mark".into();
        assert_eq!(save_block(&s, false), None);
        s.categories.clear();
        assert_eq!(
            save_block(&s, true),
            Some(crate::tr_str(true, "sketch_edit.p_tick_at_least_one_category"))
        );
    }

    #[test]
    fn yes_no_not_set_picks_commit_yes_no_and_empty() {
        assert_eq!(
            [yn_commit(0), yn_commit(1), yn_commit(2)],
            ["yes", "no", ""]
        );
        assert_eq!(yn_index(&Some(ddc_model::PVal::Bool(true))), 0);
        assert_eq!(yn_index(&Some(ddc_model::PVal::Bool(false))), 1);
        assert_eq!(yn_index(&None), 2);
        // the committed text lands in the model as true, false and no value
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        let mut s = blank();
        s.name = "Checked".into();
        s.ptype = ParamType::YesNo;
        let pid = e.model.add_param(&s).unwrap();
        e.reload().unwrap();
        let w = e.walls[0].id;
        for (idx, want) in [
            (0, Some(ddc_model::PVal::Bool(true))),
            (1, Some(ddc_model::PVal::Bool(false))),
            (2, None),
        ] {
            crate::schedule::extra::edit_param(&mut e, pid, "wall", w, yn_commit(idx));
            assert_eq!(e.model.param_value(pid, "wall", w).unwrap(), want);
        }
    }

    #[test]
    fn not_set_caption_exists_in_both_languages() {
        for key in ["params_list.not_set", "params_list.list_title", "params_list.shared_menu"] {
            let (en, ru) = (crate::tr_str(false, key), crate::tr_str(true, key));
            assert_ne!(en, key);
            assert_ne!(ru, key);
            assert_ne!(en, ru, "{key}");
        }
    }

    #[test]
    fn form_draws_in_both_languages_for_every_type() {
        let th = Theme::new(ddc_ui::Mode::Light, ddc_ui::Density::Compact);
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        for ru in [false, true] {
            for t in ParamType::ALL {
                let mut s = blank();
                s.ptype = t;
                let _ = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| form(ui, &th, &mut s, ru));
                });
                assert_eq!(s.ptype, t);
            }
        }
    }

    /// Context with the named font weights: a bare one panics on the first labelled button.
    fn font_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        ctx
    }

    struct Rig {
        ctx: egui::Context,
        ed: Editor,
        toasts: ddc_ui::notify::Toasts,
        screen: egui::Vec2,
    }

    impl Rig {
        fn new(screen: egui::Vec2) -> Self {
            Rig {
                ctx: font_ctx(),
                ed: Editor::new_in_memory().unwrap(),
                toasts: Default::default(),
                screen,
            }
        }

        fn frame(&mut self, ru: bool, events: Vec<egui::Event>) {
            SEEN.with(|s| s.borrow_mut().clear());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
                events,
                ..Default::default()
            };
            let (ed, toasts) = (&mut self.ed, &mut self.toasts);
            let _ = self.ctx.run(input, |ctx| window(ctx, ed, toasts, ru));
        }

        fn settle(&mut self, ru: bool) {
            for _ in 0..6 {
                self.frame(ru, vec![]);
            }
        }

        fn win_rect(&self) -> egui::Rect {
            let id = egui::Id::new(("ddc.dialog", "project_parameters"));
            let a = egui::AreaState::load(&self.ctx, id).expect("window state");
            egui::Rect::from_min_size(a.left_top_pos(), a.size.expect("window size"))
        }

        fn rect_of(tag: &str) -> egui::Rect {
            SEEN.with(|s| {
                s.borrow()
                    .iter()
                    .find(|t| t.0 == tag)
                    .unwrap_or_else(|| panic!("{tag} not drawn"))
                    .1
            })
        }

        fn click(&mut self, ru: bool, at: egui::Pos2) {
            let btn = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            self.frame(ru, vec![egui::Event::PointerMoved(at)]);
            self.frame(ru, vec![btn(true)]);
            self.frame(ru, vec![btn(false)]);
            self.settle(ru);
        }
    }

    fn open_fresh(name: &str) {
        WIN.with(|w| {
            let mut w = w.borrow_mut();
            *w = WinState::default();
            w.open = true;
            w.form.1.name = name.into();
        });
    }

    #[test]
    fn dialog_content_fits_window_at_default_and_minimum_size_in_both_languages() {
        // the minimum window is the screen minus the dialog margin: 336 x 200
        let sizes = [egui::vec2(1280.0, 720.0), egui::vec2(368.0, 232.0)];
        for ru in [false, true] {
            for screen in sizes {
                open_fresh("Mark");
                let mut rig = Rig::new(screen);
                rig.settle(ru);
                let win = rig.win_rect();
                let save = Rig::rect_of("save");
                assert!(win.contains_rect(save), "Save {save:?} outside {win:?} ru={ru} {screen:?}");
                for (c, _) in PARAM_CATEGORIES {
                    let r = Rig::rect_of(c);
                    assert!(
                        r.left() >= win.left() && r.right() <= win.right(),
                        "{c} {r:?} outside {win:?} ru={ru} {screen:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn save_click_writes_the_parameter() {
        open_fresh("  Mark ");
        let mut rig = Rig::new(egui::vec2(1280.0, 720.0));
        rig.settle(false);
        let at = Rig::rect_of("save").center();
        rig.click(false, at);
        let defs = rig.ed.model.params().unwrap();
        assert_eq!(defs.len(), 1, "Save writes one parameter");
        let s = &defs[0].spec;
        assert_eq!(s.name, "Mark");
        assert_eq!(s.ptype, ParamType::Text);
        assert_eq!(s.categories, vec!["wall".to_string()]);
        assert!(WIN.with(|w| w.borrow().form.0.is_none()));
    }

    /// Wheel over the list until it rests at the bottom.
    fn scroll_list_to_end(rig: &mut Rig, ru: bool) {
        let at = Rig::rect_of("cat_list").center();
        rig.frame(ru, vec![egui::Event::PointerMoved(at)]);
        for _ in 0..80 {
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -200.0),
                modifiers: egui::Modifiers::NONE,
            };
            rig.frame(ru, vec![wheel]);
        }
        rig.settle(ru);
    }

    #[test]
    fn category_list_ends_above_the_pinned_footer_in_both_languages() {
        for ru in [false, true] {
            for screen in [egui::vec2(1280.0, 720.0), egui::vec2(368.0, 232.0)] {
                open_fresh("Mark");
                let mut rig = Rig::new(screen);
                rig.settle(ru);
                let list = Rig::rect_of("cat_list");
                let save = Rig::rect_of("save");
                assert!(
                    list.bottom() <= save.top() + 0.5,
                    "list {list:?} reaches footer {save:?} ru={ru} {screen:?}"
                );
            }
        }
    }

    #[test]
    fn last_category_row_is_fully_visible_after_scrolling_in_both_languages() {
        for ru in [false, true] {
            open_fresh("Mark");
            let mut rig = Rig::new(egui::vec2(1280.0, 720.0));
            rig.settle(ru);
            let first = PARAM_CATEGORIES[0].0;
            assert!(Rig::rect_of(first).bottom() <= Rig::rect_of("cat_list").bottom() + 0.5);
            scroll_list_to_end(&mut rig, ru);
            let list = Rig::rect_of("cat_list");
            let last = Rig::rect_of(PARAM_CATEGORIES[PARAM_CATEGORIES.len() - 1].0);
            assert!(
                last.top() >= list.top() - 0.5 && last.bottom() <= list.bottom() + 0.5,
                "last row {last:?} not inside list {list:?} after scroll ru={ru}"
            );
            assert!(Rig::rect_of(first).bottom() < list.top(), "list did not scroll ru={ru}");
        }
    }

    #[test]
    fn scroll_list_with_many_rows_is_capped_and_with_few_rows_shrinks() {
        let ctx = font_ctx();
        let th = ddc_ui::Theme::new(ddc_ui::theme::Mode::Dark, ddc_ui::theme::Density::Compact);
        let row_h = th.d().control;
        for (rows, capped) in [(3usize, false), (20, true)] {
            let mut out = egui::Rect::NOTHING;
            for _ in 0..4 {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 600.0),
                    )),
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let r = ui.scope(|ui| {
                            kit::scroll_list(ui, "t", 4, row_h, |ui| {
                                for i in 0..rows {
                                    ui.add_sized([60.0, row_h], egui::Label::new(i.to_string()));
                                }
                            })
                        });
                        out = r.response.rect;
                    });
                });
            }
            let cap = 4.0 * row_h + 3.0 * 4.0;
            if capped {
                assert!(out.height() <= cap + 8.0, "{out:?} above cap {cap}");
            } else {
                assert!(out.height() < cap, "few rows should shrink: {out:?}");
            }
        }
    }

    #[test]
    fn untick_with_values_still_asks_before_saving_from_the_window() {
        let mut rig = Rig::new(egui::vec2(1280.0, 720.0));
        let m = &mut rig.ed.model;
        let lv = m.add_level("L1", 0.0).unwrap();
        let t = m
            .add_wall_type(
                "w",
                &[ddc_model::LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let wall = m
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let door = m
            .add_opening(
                wall,
                &ddc_model::OpeningSpec {
                    along: 0.5,
                    width: 0.9,
                    sill: 0.0,
                    height: 2.1,
                    kind: "door".into(),
                },
            )
            .unwrap();
        let mut spec = blank();
        spec.name = "Mark".into();
        spec.categories = vec!["wall".into(), "door".into()];
        let pid = m.add_param(&spec).unwrap();
        m.set_param_values(&[ddc_model::ParamSet {
            param: pid,
            kind: "opening".into(),
            id: door,
            value: Some(ddc_model::PVal::Text("V".into())),
        }])
        .unwrap();
        // editing: the door category is off in the form
        spec.categories = vec!["wall".into()];
        open_fresh("");
        WIN.with(|w| w.borrow_mut().form = (Some(pid), spec));
        rig.settle(false);
        let at = Rig::rect_of("save").center();
        rig.click(false, at);
        assert!(WIN.with(|w| w.borrow().drop.is_some()), "confirm is parked");
        assert_eq!(rig.ed.model.param_values(pid).unwrap().len(), 1, "nothing removed yet");
    }

    #[test]
    fn params_window_sizes_come_from_tokens() {
        let src = include_str!("params_ui.rs");
        let prod = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for call in ["vec2(", "add_space(", "set_min_width(", "set_width(", "set_min_height(", "max_width("] {
            for (at, _) in prod.match_indices(call) {
                let next = prod[at + call.len()..].chars().next().unwrap_or(' ');
                assert!(!next.is_ascii_digit(), "literal size after {call} in params_ui.rs");
            }
        }
    }

    #[test]
    fn params_window_uses_no_raw_widgets_or_literal_colours() {
        let src = include_str!("params_ui.rs");
        let prod = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for pat in [
            "ui.button(",
            ".small_button(",
            "egui::Button",
            "ComboBox",
            "selectable_label",
            "selectable_value",
            "TextEdit",
            "text_edit_singleline",
            "DragValue",
            "ui.checkbox(",
            ".separator()",
            "menu_button(",
            "toggle_value(",
            "radio_value(",
            "egui::Checkbox",
            "egui::Grid",
            "ScrollArea",
            "Color32::",
            "from_rgb",
        ] {
            assert!(!prod.contains(pat), "raw widget or colour {pat} in params_ui.rs");
        }
    }

    #[test]
    fn shared_import_is_one_undo_and_skips_existing() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut s = blank();
        s.name = "Mark".into();
        e.model.add_param(&s).unwrap();
        let mut b = blank();
        b.name = "Price".into();
        b.ptype = ParamType::Number;
        b.unit = "pcs".into();
        // "Mark" as stored: add_param gave it a guid, and the file must carry that same guid
        let mut defs = e.model.params().unwrap();
        assert!(defs[0].spec.guid.is_some());
        defs.push(ParamDef { id: 2, spec: b });
        let text = ddc_model::shared_text(&defs);
        let r = import_shared(&mut e, &text).unwrap();
        assert_eq!((r.added, r.same, r.conflicts.len()), (1, 1, 0));
        assert_eq!(e.model.params().unwrap().len(), 2);
        e.undo();
        assert_eq!(e.model.params().unwrap().len(), 1);
        assert!(import_shared(&mut e, "not a file").is_err());
    }

    #[test]
    fn shared_import_failure_adds_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let h = ddc_model::SHARED_HEADER;
        // a second one with the same guid rejects the whole import; no undo step
        let text = format!(
            "{h}\n0000-1\tA\ttext\t\t\tinstance\twall\n0000-1\tB\ttext\t\t\tinstance\twall\n"
        );
        let undo = e.can_undo();
        assert!(import_shared(&mut e, &text).is_err());
        assert!(e.model.params().unwrap().is_empty());
        assert_eq!(e.can_undo(), undo);
    }

    fn shared_file(rows: &[&str]) -> String {
        format!("{}\n{}\n", ddc_model::SHARED_HEADER, rows.join("\n"))
    }

    #[test]
    fn import_shared_same_name_other_type_is_conflict() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut s = blank();
        s.name = "Mark".into();
        e.model.add_param(&s).unwrap();
        let text = shared_file(&["0000-9\tMark\tnumber\t\t\tinstance\twall"]);
        let r = import_shared(&mut e, &text).unwrap();
        assert_eq!((r.added, r.same), (0, 0));
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].name, "Mark");
        assert_eq!(r.conflicts[0].file.ptype, ParamType::Number);
        assert_eq!(r.conflicts[0].model.ptype, ParamType::Text);
        assert_eq!(e.model.params().unwrap().len(), 1);
    }

    #[test]
    fn import_shared_renamed_guid_not_refused() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut s = blank();
        s.name = "Old".into();
        s.guid = Some("0000-7".into());
        e.model.add_param(&s).unwrap();
        let text = shared_file(&["0000-7\tNew\ttext\t\t\tinstance\twall"]);
        let undo = e.can_undo();
        let r = import_shared(&mut e, &text).unwrap();
        assert_eq!(r.renamed, vec![("New".to_string(), "Old".to_string())]);
        assert_eq!((r.added, r.same, r.conflicts.len()), (0, 1, 0));
        assert_eq!(e.model.params().unwrap().len(), 1);
        assert_eq!(e.can_undo(), undo);
    }

    #[test]
    fn import_shared_identical_is_same() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut s = blank();
        s.name = "Mark".into();
        s.guid = Some("0000-3".into());
        e.model.add_param(&s).unwrap();
        let text = shared_file(&["0000-3\tmark\ttext\t\t\tinstance\twall"]);
        let r = import_shared(&mut e, &text).unwrap();
        assert_eq!((r.added, r.same, r.conflicts.len()), (0, 1, 0));
        assert!(r.renamed.is_empty());
    }

    #[test]
    fn import_shared_mixed_file_is_one_undo_step() {
        let mut e = Editor::new_in_memory().unwrap();
        let mut a = blank();
        a.name = "Mark".into();
        e.model.add_param(&a).unwrap();
        let text = shared_file(&[
            "0000-1\tMark\tnumber\t\t\tinstance\twall",
            "0000-2\tAlpha\ttext\t\t\tinstance\twall",
            "0000-3\tBeta\ttext\t\t\tinstance\twall",
        ]);
        let r = import_shared(&mut e, &text).unwrap();
        assert_eq!((r.added, r.conflicts.len()), (2, 1));
        assert_eq!(e.model.params().unwrap().len(), 3);
        e.undo();
        assert_eq!(e.model.params().unwrap().len(), 1);
    }

    #[test]
    fn import_shared_unreadable_writes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let undo = e.can_undo();
        assert!(import_shared(&mut e, "garbage").is_err());
        assert!(e.model.params().unwrap().is_empty());
        assert_eq!(e.can_undo(), undo);
    }

    #[test]
    fn props_value_edit_goes_through_model() {
        let mut e = Editor::new_in_memory().unwrap();
        crate::schedule::test_room(&mut e);
        let mut s = blank();
        s.name = "Fire rating".into();
        let pid = e.model.add_param(&s).unwrap();
        e.reload().unwrap();
        let w = e.walls[0].id;
        crate::schedule::extra::edit_param(&mut e, pid, "wall", w, "EI 60");
        assert_eq!(
            e.model.param_value(pid, "wall", w).unwrap(),
            Some(ddc_model::PVal::Text("EI 60".into()))
        );
        e.selection = vec![crate::editor::Sel::Wall(w)];
        let ctx = egui::Context::default();
        // the panel uses the named font weights: they exist from the pass after install
        ddc_ui::theme::install_fonts(&ctx);
        let _ = ctx.run(Default::default(), |_| {});
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| props(ui, &mut e, true));
        });
        e.undo();
        assert_eq!(e.model.param_value(pid, "wall", w).unwrap(), None);
    }

    fn one_param_with_value(e: &mut Editor) -> i64 {
        let lv = e.model.add_level("L1", 0.0).unwrap();
        let t = e
            .model
            .add_wall_type(
                "w",
                &[ddc_model::LayerSpec {
                    material: "brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = e
            .model
            .add_wall(&ddc_model::WallSpec {
                a: (0.0, 0.0),
                b: (5.0, 0.0),
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: t,
                offset0: None,
            })
            .unwrap();
        let mut s = blank();
        s.name = "Mark".into();
        let pid = e.model.add_param(&s).unwrap();
        e.model
            .set_param_values(&[ddc_model::ParamSet {
                param: pid,
                kind: "wall".into(),
                id: w,
                value: Some(ddc_model::PVal::Text("A1".into())),
            }])
            .unwrap();
        pid
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ddc_ux31a_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn export_shared_reports_count() {
        let mut e = Editor::new_in_memory().unwrap();
        one_param_with_value(&mut e);
        let d = scratch("count");
        let p = d.join("shared.txt");
        for ru in [false, true] {
            let line = export_shared(&mut e, &p, ru).unwrap();
            assert!(line.contains("shared.txt") && line.contains('1'), "{line}");
        }
        assert!(std::fs::read_to_string(&p).unwrap().contains("Mark"));
        // re-export over the user's file leaves nothing beside it
        assert!(!d.join("shared.txt.bak").exists());
        assert!(!d.join("shared.txt.part").exists());
    }

    #[test]
    fn export_shared_failed_write_keeps_old_file() {
        let mut e = Editor::new_in_memory().unwrap();
        one_param_with_value(&mut e);
        let d = scratch("keep");
        let p = d.join("shared.txt");
        std::fs::write(&p, "old").unwrap();
        // a directory in the part file's place makes the write fail before the rename
        std::fs::create_dir(d.join("shared.txt.part")).unwrap();
        assert!(export_shared(&mut e, &p, false).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "old");
    }

    #[test]
    fn export_shared_permission_error_humanized() {
        let raw = std::io::Error::from_raw_os_error(5);
        for ru in [false, true] {
            let t = io_text(&raw, ru);
            assert!(!t.contains("os error"), "{t}");
            assert!(!t.is_empty());
        }
    }

    #[test]
    fn delete_param_first_click_changes_nothing() {
        let mut e = Editor::new_in_memory().unwrap();
        let pid = one_param_with_value(&mut e);
        let mut armed = None;
        assert!(!click_delete(&mut armed, pid));
        assert_eq!(armed, Some(pid));
        assert_eq!(e.model.params().unwrap().len(), 1);
        assert_eq!(e.model.param_usage(pid).unwrap().0, 1);
        assert!(click_delete(&mut armed, pid));
    }

    #[test]
    fn delete_param_confirm_text_has_value_count() {
        let mut e = Editor::new_in_memory().unwrap();
        let pid = one_param_with_value(&mut e);
        let d = e.model.params().unwrap().remove(0);
        assert_eq!(d.id, pid);
        for ru in [false, true] {
            let t = confirm_text(&e, &d, ru);
            assert!(t.contains("Mark") && t.contains("1"), "{t}");
            assert!(!t.contains('{'), "{t}");
        }
    }

    #[test]
    fn delete_param_toast_has_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let pid = one_param_with_value(&mut e);
        let mut toasts = ddc_ui::notify::Toasts::default();
        delete_confirmed(&mut e, &mut toasts, false, pid).unwrap();
        assert!(e.model.params().unwrap().is_empty());
        let t = toasts.live.last().unwrap();
        assert_eq!(t.command.as_deref(), Some("edit.undo"));
        assert!(t.entry.is_some() && t.entry == e.top_entry());
        e.undo();
        assert_eq!(e.model.params().unwrap().len(), 1);
        assert_eq!(e.model.param_usage(pid).unwrap().0, 1);
    }
}
