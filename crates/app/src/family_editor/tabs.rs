// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Family editor tabs. Each edit is a document operator (`FamDoc`), validated by
//! parsing; an error is shown in the message line and the text is unchanged.

use ddc_geom::component::doc::{deg, mm, RefAt};
use ddc_geom::component::kernel::{BulgeProfile, Own, SolidOp};
use ddc_geom::component::{catalog, flex, Axis, Family, Kind, Value};
use ddc_geom::gsolid::{Form, Frame};
use ddc_geom::{v2, v3};
use ddc_ui::theme::space;
use ddc_ui::widgets::field::{format_number, parse_number, NumberFormat};
use ddc_ui::widgets::{self, Button, Cell, Column, MenuTone, RowState, Table};
use ddc_ui::{Catalog, Theme};
use egui::{self, RichText};

use super::{parse_num, Action, Project, Session, Tab};

pub const KINDS: [Kind; 10] = [
    Kind::Length,
    Kind::Angle,
    Kind::Area,
    Kind::Volume,
    Kind::Number,
    Kind::Int,
    Kind::Bool,
    Kind::Text,
    Kind::Material,
    Kind::FamilyType,
];

/// Id of the parameter name field: the empty state's action focuses it.
const PARAM_NAME_FIELD: &str = "g11.fe.param_name_field";
const AXES: [Axis; 3] = [Axis::X, Axis::Y, Axis::Z];

fn kind_text(cat: &Catalog, k: Kind) -> String {
    cat.t(&format!("fe.kind.{}", k.name()))
}

fn is_text(k: Kind) -> bool {
    matches!(k, Kind::Text | Kind::Material | Kind::FamilyType)
}

pub fn value_text(k: Kind, v: &Value) -> String {
    match v {
        Value::Text(t) => format!("\"{t}\""),
        Value::Num(x) => match k {
            Kind::Length => mm(*x),
            Kind::Angle => deg(*x),
            Kind::Area => format!("{x}m2"),
            Kind::Volume => format!("{x}m3"),
            _ => format!("{x}"),
        },
    }
}

/// Tail of the `param` operator: a formula (`= ...`) or a value (texts are quoted).
fn param_tail(kind: Kind, v: &str) -> String {
    let v = v.trim();
    if v.starts_with('=') || !is_text(kind) || v.starts_with('"') {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('"', "'"))
    }
}

fn hint(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, key: &str) {
    hint_text(ui, th, &cat.t(key));
}

fn hint_text(ui: &mut egui::Ui, th: &Theme, text: &str) {
    crate::ai::ui::kit::caption(ui, th, text);
}

/// Locale key of the axis label.
pub fn axis_key(a: Axis) -> &'static str {
    match a {
        Axis::X => "fe.axis.x",
        Axis::Y => "fe.axis.y",
        Axis::Z => "fe.axis.z",
    }
}

fn axis_text(cat: &Catalog, a: Axis) -> String {
    cat.t(axis_key(a))
}

/// Type table cell: the type's value or the one inherited from the parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Typed,
    Inherited,
}

pub fn cell_kind(typed: Option<&Value>) -> CellKind {
    if typed.is_some() {
        CellKind::Typed
    } else {
        CellKind::Inherited
    }
}

/// Width of the numeric column in tab tables.
const NUM_COL_W: f32 = 104.0;

/// Numeric column: right-aligned, unsorted.
fn num_col(title: &str) -> Column<'_> {
    Column::number(title, NUM_COL_W).sortable(false)
}

fn text_col(title: &str) -> Column<'_> {
    Column::text(title).sortable(false)
}

/// A number with locale separators, without trailing zeros in the fraction.
pub fn fmt_num(v: f64, max_decimals: usize, nf: NumberFormat) -> String {
    let s = format_number(v, max_decimals, nf);
    if max_decimals == 0 || !s.contains(nf.decimal) {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches(nf.decimal).to_string()
}

/// Value for a numeric cell: the number text and a unit (length in project units).
pub fn num_parts(k: Kind, v: &Value, imperial: bool, nf: NumberFormat) -> (String, &'static str) {
    let x = match v {
        Value::Text(t) => return (t.clone(), ""),
        Value::Num(x) => *x,
    };
    match k {
        Kind::Length if imperial => (fmt_num(x / 0.0254, 3, nf), "in"),
        Kind::Length => (fmt_num(x * 1000.0, 3, nf), "mm"),
        Kind::Angle => (fmt_num(x.to_degrees(), 2, nf), "\u{b0}"),
        Kind::Area => (fmt_num(x, 4, nf), "m\u{b2}"),
        Kind::Volume => (fmt_num(x, 4, nf), "m\u{b3}"),
        Kind::Int => (fmt_num(x, 0, nf), ""),
        _ => (fmt_num(x, 4, nf), ""),
    }
}

/// Action button: secondary, with disabling and a reason on hover.
fn btn(ui: &mut egui::Ui, th: &Theme, label: &str) -> egui::Response {
    Button::secondary(label).show(ui, th)
}

/// "Add": a secondary button with a plus icon.
fn add_btn(ui: &mut egui::Ui, th: &Theme, label: &str) -> bool {
    let ctx = ui.ctx().clone();
    widgets::with_icons(&ctx, |icons| {
        Button::secondary(label).show_icon(ui, th, icons, "plus")
    })
    .clicked()
}

/// A dropdown by strings: the selected string changes in place.
fn combo(ui: &mut egui::Ui, th: &Theme, id: &str, value: &mut String, items: &[String]) {
    let mut idx = items.iter().position(|i| i == value).unwrap_or(usize::MAX);
    let refs: Vec<&str> = items.iter().map(String::as_str).collect();
    if widgets::dropdown(ui, th, id, &mut idx, &refs, None).changed() {
        if let Some(it) = items.get(idx) {
            *value = it.clone();
        }
    }
}

/// A dropdown by index.
fn pick_idx(ui: &mut egui::Ui, th: &Theme, id: &str, sel: &mut usize, labels: &[String]) {
    *sel = (*sel).min(labels.len().saturating_sub(1));
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    widgets::dropdown(ui, th, id, sel, &refs, None);
}

/// Popup menu rows under `resp`; returns the chosen one.
fn menu_rows(resp: &egui::Response, th: &Theme, items: &[String]) -> Option<usize> {
    let mut pick = None;
    egui::Popup::menu(resp).show(|ui| {
        for (i, it) in items.iter().enumerate() {
            if widgets::menu_item(ui, th, it, "", MenuTone::Normal, true).clicked() {
                pick = Some(i);
            }
        }
    });
    pick
}

/// A button with a choice menu; disabled when it has no items.
fn pick_menu(ui: &mut egui::Ui, th: &Theme, label: &str, items: &[String]) -> Option<usize> {
    let r = ui
        .add_enabled_ui(!items.is_empty(), |ui| btn(ui, th, label))
        .inner;
    menu_rows(&r, th, items)
}

fn section_gap(ui: &mut egui::Ui) {
    ui.add_space(space::SM);
    ui.separator();
    ui.add_space(space::SM);
}

fn plane_names(f: &Family) -> Vec<String> {
    f.refs
        .iter()
        .map(|r| r.name.clone())
        .chain(f.ext.free_refs.iter().map(|r| r.name.clone()))
        .chain(f.ext.lines.iter().map(|l| l.name.clone()))
        .collect()
}

pub fn show(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    project: &Project,
    acts: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing = egui::vec2(space::SM, space::SM);
    if s.tab == Tab::Text {
        return text(ui, th, cat, s);
    }
    let Some(f) = s.eval.family.clone() else {
        hint(ui, th, cat, "fe.fix_text_first");
        return text(ui, th, cat, s);
    };
    match s.tab {
        Tab::Params => params(ui, th, cat, s, &f),
        Tab::Types => types(ui, th, cat, s, &f, project),
        Tab::Constraints => constraints(ui, th, cat, s, &f, project),
        Tab::Solids => solids(ui, th, cat, s, &f, project),
        Tab::Visibility => visibility(ui, th, cat, s, &f),
        Tab::Nested => nested(ui, th, cat, s, &f, project, acts),
        Tab::Flex => flex_tab(ui, th, cat, s, &f, project),
        Tab::Text => {}
    }
}

/// Parameter value in the table: a formula is shown evaluated, marked with "ƒ".
fn param_shown(s: &Session, f: &Family, p: &ddc_geom::component::Param) -> String {
    match (&p.formula, s.eval.resolved.as_ref()) {
        (Some(_), Some(r)) => r
            .num(&p.name)
            .map(|v| format!("ƒ {}", value_text(p.kind, &Value::Num(v))))
            .or_else(|| r.texts.get(&p.name).map(|t| format!("ƒ \"{t}\"")))
            .unwrap_or_else(|| "ƒ".into()),
        _ if f.ext.text_formulas.iter().any(|t| t.0 == p.name) => s
            .eval
            .resolved
            .as_ref()
            .and_then(|r| r.texts.get(&p.name))
            .map(|t| format!("ƒ \"{t}\""))
            .unwrap_or_else(|| "ƒ".into()),
        _ => value_text(p.kind, &p.default),
    }
}

/// A table row is selected: the form fields take the parameter's values.
fn select_param(s: &mut Session, p: &ddc_geom::component::Param) {
    s.f.p_name = p.name.clone();
    s.f.p_kind = KINDS.iter().position(|k| *k == p.kind).unwrap_or(0);
    s.f.p_inst = p.instance;
    let st = s.doc.find("param", &p.name);
    // the tail as in the text: after "param name kind scope"
    s.f.p_val = st
        .and_then(|st| {
            let w: Vec<&str> = st.text.splitn(5, char::is_whitespace).collect();
            w.get(4).map(|x| x.trim().to_string())
        })
        .unwrap_or_default();
}

fn params_table(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session, f: &Family) {
    let titles = ["fe.col.name", "fe.col.kind", "fe.col.scope", "fe.col.value"].map(|h| cat.t(h));
    let cols = [
        text_col(&titles[0]),
        text_col(&titles[1]).fixed(110.0),
        text_col(&titles[2]).fixed(90.0),
        text_col(&titles[3]).fixed(150.0),
    ];
    let kinds: Vec<String> = f.params.iter().map(|p| kind_text(cat, p.kind)).collect();
    let scopes = [cat.t("fe.scope.type"), cat.t("fe.scope.instance")];
    let shown: Vec<String> = f.params.iter().map(|p| param_shown(s, f, p)).collect();
    let mut sort = None;
    let out = Table::new("g11.fe.params", &cols, f.params.len())
        .max_height(240.0)
        .show(
            ui,
            th,
            &mut sort,
            |r| {
                if s.f.p_name == f.params[r].name {
                    RowState::Selected
                } else {
                    RowState::Normal
                }
            },
            |r, c| match c {
                0 => Cell::text(f.params[r].name.clone()),
                1 => Cell::text(kinds[r].clone()),
                2 => Cell::text(scopes[usize::from(f.params[r].instance)].clone()),
                _ => Cell::text(shown[r].clone()),
            },
        );
    let cur = f.params.iter().position(|p| p.name == s.f.p_name);
    let pick = out.clicked.or_else(|| {
        out.moved.map(|d| {
            let at = cur.map_or(-1, |c| c as isize) + d;
            at.clamp(0, f.params.len() as isize - 1) as usize
        })
    });
    if let Some(p) = pick.and_then(|i| f.params.get(i)) {
        select_param(s, p);
    }
}

fn params(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session, f: &Family) {
    if let Some(k) = crate::empty_kind::family_params(f.params.len()) {
        // the compact card already sets the form apart: the form follows right under it
        crate::ui_kit::empty_state_compact_lang(ui, crate::ui_lang_now(), k.what());
    } else {
        params_table(ui, th, cat, s, f);
        section_gap(ui);
    }
    ui.label(RichText::new(cat.t("fe.param.form")).strong());
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.f.p_name)
                .id(egui::Id::new(PARAM_NAME_FIELD))
                .hint_text(cat.t("fe.col.name"))
                .desired_width(110.0),
        );
        let kinds: Vec<String> = KINDS.iter().map(|k| kind_text(cat, *k)).collect();
        pick_idx(ui, th, "g11.fe.pkind", &mut s.f.p_kind, &kinds);
        widgets::checkbox(ui, th, &mut s.f.p_inst, &cat.t("fe.scope.instance"));
    });
    ui.add(
        egui::TextEdit::singleline(&mut s.f.p_val)
            .hint_text(cat.t("fe.param.value_hint"))
            .desired_width(f32::INFINITY),
    );
    hint(ui, th, cat, "fe.param.formula_help");
    ui.horizontal(|ui| {
        let kind = KINDS[s.f.p_kind.min(KINDS.len() - 1)];
        let name = s.f.p_name.trim().to_string();
        let tail = param_tail(kind, &s.f.p_val);
        let exists = f.params.iter().any(|p| p.name == name);
        if gated_button(ui, th, cat, "fe.add", add_why(&name, exists)) {
            let r = s.doc.add_param(&name, kind, s.f.p_inst, &tail);
            s.apply(cat, r);
        }
        if gated_button(ui, th, cat, "fe.apply", param_edit_why(exists)) {
            let r = s.doc.set_param(&name, kind, s.f.p_inst, &tail);
            s.apply(cat, r);
        }
        if gated_button(ui, th, cat, "fe.remove", param_edit_why(exists)) {
            if let Some(st) = s.doc.find("param", &name) {
                let r = s.doc.remove_stmt(&st);
                s.apply(cat, r);
            }
        }
    });
    section_gap(ui);
    // title, axes and Apply on one row (help on hover): at the default window height the
    // inputs on a third line were cut off
    let help = cat.t("fe.origin.help");
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(cat.t("fe.origin")).strong()).on_hover_text(&help);
        for (i, a) in AXES.iter().enumerate() {
            ui.label(axis_text(cat, *a));
            ui.add(egui::TextEdit::singleline(&mut s.f.o[i]).desired_width(70.0))
                .on_hover_text(&help);
        }
        if btn(ui, th, &cat.t("fe.apply")).clicked() {
            let [x, y, z] = s.f.o.clone();
            let r = s.doc.set_origin(&x, &y, &z);
            s.apply(cat, r);
        }
    });
}

fn types(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
) {
    let pcols = catalog::columns(f);
    let nf = NumberFormat::for_lang(cat.lang);
    let type_title = cat.t("fe.col.type");
    let mut cols = vec![text_col(&type_title)];
    cols.extend(pcols.iter().map(|p| {
        if is_text(p.kind) {
            text_col(&p.name).fixed(NUM_COL_W)
        } else {
            num_col(&p.name)
        }
    }));
    let inherited_tip = cat.t("fe.types.inherited");
    let mut sort = None;
    Table::new("g11.fe.types", &cols, f.types.len())
        .max_height(260.0)
        .show(
            ui,
            th,
            &mut sort,
            |_| RowState::Normal,
            |r, c| {
                let t = &f.types[r];
                let Some(p) = c.checked_sub(1).and_then(|i| pcols.get(i)) else {
                    return Cell::text(t.name.clone());
                };
                let typed = t.values.iter().find(|v| v.0 == p.name).map(|v| &v.1);
                let kind = cell_kind(typed);
                let (txt, unit) =
                    num_parts(p.kind, typed.unwrap_or(&p.default), project.imperial, nf);
                let cell = if is_text(p.kind) {
                    Cell::text(txt)
                } else {
                    Cell::num(txt, unit)
                };
                match kind {
                    CellKind::Typed => cell,
                    CellKind::Inherited => {
                        let mut c = cell.with_tip(inherited_tip.clone());
                        c.muted = true;
                        c
                    }
                }
            },
        );
    section_gap(ui);
    hint(ui, th, cat, "fe.csv.help");
    ui.horizontal(|ui| {
        let more = cat.t("fe.csv.more");
        let ctx = ui.ctx().clone();
        let menu_btn = widgets::with_icons(&ctx, |icons| {
            widgets::icon_button(ui, icons, th, "context-menu", false, &more)
        });
        let rows = [
            cat.t("fe.csv.export"),
            cat.t("fe.csv.copy"),
            cat.t("fe.csv.save_file"),
            cat.t("fe.csv.open_file"),
        ];
        match menu_rows(&menu_btn, th, &rows) {
            Some(0) => s.f.csv = catalog::export_csv(f, &f.types),
            Some(1) => ui.ctx().copy_text(catalog::export_csv(f, &f.types)),
            Some(2) => {
                if let Some(p) = crate::file_pick::Dlg::new()
                    .add_filter("CSV", &["csv"])
                    .set_file_name(format!("{}.csv", f.name))
                    .save_file()
                {
                    let r = ddc_io::fsutil::replace_durably_bytes(
                        &p,
                        catalog::export_csv_file(f, &f.types, crate::ui_is_ru()).as_bytes(),
                    );
                    s.msg = r.err().map(|e| (true, e));
                }
            }
            Some(3) => {
                if let Some(p) = crate::file_pick::Dlg::new()
                    .add_filter("CSV", &["csv", "txt"])
                    .pick_file()
                {
                    match std::fs::read_to_string(&p) {
                        Ok(t) => s.f.csv = catalog::strip_bom(&t).to_string(),
                        Err(e) => s.msg = Some((true, e.to_string())),
                    }
                }
            }
            _ => {}
        }
        if btn(ui, th, &cat.t("fe.csv.import")).clicked() {
            // plan and preview first: an empty cell and no column leave the value as it is
            s.f.csv_preview = None;
            match super::catalog_import::plan(f, &s.f.csv, project.length_unit()) {
                Ok(plan) if plan.is_empty() => {
                    let mut m = cat.t("fe.csv.nothing");
                    for l in super::catalog_import::lines(cat, f, &plan, project.imperial, nf)
                        .into_iter()
                        .skip(1)
                    {
                        m += "\n";
                        m += &l;
                    }
                    s.msg = Some((!plan.skipped.is_empty(), m));
                }
                Ok(plan) => {
                    s.msg = None;
                    s.f.csv_preview = Some(super::catalog_import::Preview {
                        csv: s.f.csv.clone(),
                        doc: s.doc.text().to_string(),
                        plan,
                    });
                }
                Err(e) => s.msg = Some((true, e.to_string())),
            }
        }
    });
    csv_preview(ui, th, cat, s, f, project.imperial, nf);
    ui.add(
        egui::TextEdit::multiline(&mut s.f.csv)
            .code_editor()
            .desired_rows(8)
            .desired_width(f32::INFINITY),
    );
}

/// Height of the change list in the catalog import preview.
const CSV_PREVIEW_H: f32 = 160.0;

/// Catalog import preview: what will change. Apply is one undo step, Cancel writes
/// nothing; editing the CSV field or the document closes a stale preview.
fn csv_preview(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    imperial: bool,
    nf: NumberFormat,
) {
    let Some(pv) = s.f.csv_preview.as_ref() else {
        return;
    };
    if !pv.current(&s.f.csv, s.doc.text()) {
        s.f.csv_preview = None;
        return;
    }
    let lines = super::catalog_import::lines(cat, f, &pv.plan, imperial, nf);
    let mut act = None;
    ui.add_space(space::SM);
    ui.group(|ui| {
        if let Some((head, rest)) = lines.split_first() {
            ui.label(RichText::new(head).strong());
            hint(ui, th, cat, "fe.csv.preview_hint");
            let out = egui::ScrollArea::vertical()
                .id_salt("fe_csv_preview")
                .max_height(CSV_PREVIEW_H)
                .show(ui, |ui| {
                    for l in rest {
                        ui.label(l);
                    }
                });
            widgets::scroll_hint::scroll_fade(ui, th, &out);
        }
        ui.horizontal(|ui| {
            if Button::primary(&cat.t("fe.csv.apply")).show(ui, th).clicked() {
                act = Some(true);
            }
            if btn(ui, th, &cat.t("fe.csv.cancel")).clicked() {
                act = Some(false);
            }
        });
    });
    match act {
        Some(true) => {
            let Some(pv) = s.f.csv_preview.take() else {
                return;
            };
            match super::catalog_import::apply(&mut s.doc, f, &pv.plan) {
                Ok(_) => {
                    s.apply(cat, Ok(()));
                    let n = pv.plan.types.len().to_string();
                    let mut m = cat.tf("fe.csv.imported", &[("n", &n)]);
                    for e in &pv.plan.skipped {
                        m += "\n";
                        m += &cat.tf("fe.csv.skipped", &[("msg", &e.to_string())]);
                    }
                    s.msg = Some((false, m));
                }
                Err(e) => s.apply(cat, Err(e)),
            }
        }
        Some(false) => s.f.csv_preview = None,
        None => {}
    }
}

/// Reference planes: a table with row selection; the "free/pinned" mode is an icon under it.
fn refs_table(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
) {
    // (name, axis, position, free)
    let rows: Vec<(String, Axis, f64, bool)> = f
        .refs
        .iter()
        .map(|p| {
            let at = s.eval.resolved.as_ref().and_then(|r| r.num(&p.name));
            (p.name.clone(), p.axis, at.unwrap_or(0.0), false)
        })
        .chain(f.ext.free_refs.iter().map(|p| {
            let at = s.eval.resolved.as_ref().and_then(|r| r.num(&p.name));
            (p.name.clone(), p.axis, at.unwrap_or(p.guess), true)
        }))
        .collect();
    if rows.is_empty() {
        return;
    }
    let sel_id = egui::Id::new("g11.fe.ref_sel");
    let sel: Option<String> = ui.data(|d| d.get_temp(sel_id));
    let nf = NumberFormat::for_lang(cat.lang);
    let titles = ["fe.col.name", "fe.col.axis", "fe.col.at", "fe.col.mode"].map(|h| cat.t(h));
    let cols = [
        text_col(&titles[0]),
        text_col(&titles[1]).fixed(90.0),
        num_col(&titles[2]),
        text_col(&titles[3]).fixed(100.0),
    ];
    let mut sort = None;
    let out = Table::new("g11.fe.refs", &cols, rows.len())
        .max_height(200.0)
        .show(
            ui,
            th,
            &mut sort,
            |r| {
                if sel.as_deref() == Some(rows[r].0.as_str()) {
                    RowState::Selected
                } else {
                    RowState::Normal
                }
            },
            |r, c| {
                let (name, axis, at, free) = &rows[r];
                match c {
                    0 => Cell::text(name.clone()),
                    1 => Cell::text(axis_text(cat, *axis)),
                    2 => {
                        let (t, u) = num_parts(Kind::Length, &Value::Num(*at), project.imperial, nf);
                        Cell::num(t, u)
                    }
                    _ => Cell::text(cat.t(if *free { "fe.mode.free" } else { "fe.mode.fixed" })),
                }
            },
        );
    let cur = rows.iter().position(|r| sel.as_deref() == Some(r.0.as_str()));
    let pick = out.clicked.or_else(|| {
        out.moved.map(|d| {
            let at = cur.map_or(-1, |c| c as isize) + d;
            at.clamp(0, rows.len() as isize - 1) as usize
        })
    });
    let sel = match pick {
        Some(i) => {
            let name = rows[i].0.clone();
            ui.data_mut(|d| d.insert_temp(sel_id, name.clone()));
            Some(name)
        }
        None => sel,
    };
    let Some((name, _, at, free)) = sel.and_then(|n| rows.iter().find(|r| r.0 == n)) else {
        return;
    };
    ui.horizontal(|ui| {
        // the toggle acts on the selected row: its tooltip names that plane
        let act = cat.t(if *free { "fe.make_fixed" } else { "fe.make_free" });
        let tip = format!("{act}: {name}");
        let ctx = ui.ctx().clone();
        let icon = if *free { "pin" } else { "unpin" };
        let clicked = widgets::with_icons(&ctx, |icons| {
            widgets::icon_button(ui, icons, th, icon, *free, &tip)
        })
        .clicked();
        hint_text(ui, th, &cat.tf("fe.ref.selected", &[("name", name.as_str())]));
        if clicked {
            let r = s.doc.set_ref_free(name, !*free, *at);
            s.apply(cat, r);
        }
    });
}

fn constraints(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
) {
    let names = plane_names(f);
    // reference planes
    ui.label(RichText::new(cat.t("fe.refs")).strong());
    refs_table(ui, th, cat, s, f, project);
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.f.r_name)
                .hint_text(cat.t("fe.col.name"))
                .desired_width(90.0),
        );
        let axes: Vec<String> = AXES.iter().map(|a| axis_text(cat, *a)).collect();
        pick_idx(ui, th, "g11.fe.raxis", &mut s.f.r_axis, &axes);
        widgets::checkbox(ui, th, &mut s.f.r_free, &cat.t("fe.free"));
        ui.add(
            egui::TextEdit::singleline(&mut s.f.r_val)
                .hint_text(cat.t("fe.ref.value_hint"))
                .desired_width(110.0),
        );
        if add_btn(ui, th, &cat.t("fe.add")) {
            let at = if s.f.r_free {
                match parse_num(&s.f.r_val) {
                    Ok(v) => RefAt::Free(v),
                    Err(e) => {
                        s.msg = Some((true, e));
                        return;
                    }
                }
            } else {
                RefAt::Fixed(s.f.r_val.trim().trim_start_matches('=').to_string())
            };
            let name = s.f.r_name.trim().to_string();
            let r = s.doc.add_ref(&name, AXES[s.f.r_axis.min(2)], at);
            s.apply(cat, r);
        }
    });
    hint(ui, th, cat, "fe.refs.help");
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.f.l_name)
                .hint_text(cat.t("fe.refline"))
                .desired_width(90.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.f.l_x)
                .hint_text("x")
                .desired_width(60.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.f.l_y)
                .hint_text("y")
                .desired_width(60.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.f.l_angle)
                .hint_text(cat.t("fe.angle_hint"))
                .desired_width(80.0),
        );
        if add_btn(ui, th, &cat.t("fe.add")) {
            let ang = if s.f.l_angle.trim().is_empty() {
                "~0deg".to_string()
            } else {
                s.f.l_angle.trim().to_string()
            };
            let line = format!(
                "refline {} at({}, {}) angle({ang})",
                s.f.l_name.trim(),
                or0(&s.f.l_x),
                or0(&s.f.l_y)
            );
            let r = s.doc.add_stmt(&line);
            s.apply(cat, r);
        }
    });
    ui.separator();
    // constraints
    ui.label(RichText::new(cat.t("fe.cons")).strong());
    let rig = s.eval.rig.clone();
    let heads = ["dim", "eq", "align", "angle"];
    // the k-th dim/eq/align/angle operator is the k-th constraint (solver report indices)
    let cons_stmts = s
        .doc
        .stmts()
        .into_iter()
        .filter(|x| heads.contains(&x.head.as_str()));
    for (k, st) in cons_stmts.enumerate() {
        ui.horizontal(|ui| {
            let flag = rig.as_ref().and_then(|g| {
                if g.conflicting_ids.iter().any(|c| c.0 == k) {
                    Some(("fe.cons.conflict", th.c("error")))
                } else if g.redundant_ids.contains(&k) {
                    Some(("fe.cons.redundant", th.c("warning")))
                } else {
                    None
                }
            });
            ui.label(RichText::new(&st.text).monospace());
            if let Some((k, c)) = flag {
                ui.label(RichText::new(cat.t(k)).color(c));
            }
            if ddc_ui::widgets::close_button(ui, &cat.t("fe.remove")).clicked() {
                let r = s.doc.remove_stmt(&st);
                s.apply(cat, r);
            }
        });
    }
    let kinds = [
        "fe.cons.dim",
        "fe.cons.eq",
        "fe.cons.align",
        "fe.cons.angle",
    ];
    ui.horizontal(|ui| {
        let klabels: Vec<String> = kinds.iter().map(|k| cat.t(k)).collect();
        pick_idx(ui, th, "g11.fe.ckind", &mut s.f.c_kind, &klabels);
        match s.f.c_kind {
            1 => {
                ui.add(
                    egui::TextEdit::singleline(&mut s.f.c_list)
                        .hint_text(cat.t("fe.cons.eq_hint"))
                        .desired_width(180.0),
                );
            }
            k => {
                if k != 2 {
                    let want = if k == 3 { Kind::Angle } else { Kind::Length };
                    let params: Vec<String> = f
                        .params
                        .iter()
                        .filter(|p| p.kind == want)
                        .map(|p| p.name.clone())
                        .collect();
                    ui.add(
                        egui::TextEdit::singleline(&mut s.f.c_val)
                            .hint_text(cat.t("fe.cons.value_hint"))
                            .desired_width(90.0),
                    );
                    if let Some(i) = pick_menu(ui, th, "ƒ", &params) {
                        s.f.c_val = params[i].clone();
                    }
                }
                combo(ui, th, "g11.fe.ca", &mut s.f.c_a, &names);
                combo(ui, th, "g11.fe.cb", &mut s.f.c_b, &names);
            }
        }
        if add_btn(ui, th, &cat.t("fe.add")) {
            let (a, b, v) = (
                s.f.c_a.clone(),
                s.f.c_b.clone(),
                s.f.c_val.trim().replace(' ', ""),
            );
            let r = match s.f.c_kind {
                0 => s.doc.add_dim(&v, &a, &b),
                1 => {
                    let l: Vec<&str> =
                        s.f.c_list
                            .split([' ', ','])
                            .filter(|x| !x.is_empty())
                            .collect();
                    let l: Vec<String> = l.into_iter().map(str::to_string).collect();
                    let refs: Vec<&str> = l.iter().map(String::as_str).collect();
                    s.doc.add_eq(&refs)
                }
                2 => s.doc.add_align(&a, &b),
                _ => s.doc.add_angle(&v, &a, &b),
            };
            s.apply(cat, r);
        }
    });
    hint(ui, th, cat, "fe.cons.help");
    if let Some(g) = &rig {
        if !g.free.is_empty() {
            ui.label(
                RichText::new(cat.tf("fe.verdict.free", &[("list", &g.free.join(", "))]))
                    .color(th.c("warning")),
            );
        }
    }
    ui.separator();
    // alignment locks: a body argument <-> a plane
    ui.label(RichText::new(cat.t("fe.locks")).strong());
    for (solid, plane) in f.locks() {
        ui.label(cat.tf("fe.lock.row", &[("solid", &solid), ("plane", &plane)]));
    }
    let solids: Vec<String> = f.solids.iter().map(|x| x.name.clone()).collect();
    ui.horizontal(|ui| {
        combo(ui, th, "g11.fe.ksolid", &mut s.f.k_solid, &solids);
        ui.add(egui::TextEdit::singleline(&mut s.f.k_clause).desired_width(60.0));
        ui.add(
            egui::DragValue::new(&mut s.f.k_arg)
                .range(0..=15)
                .prefix("#"),
        );
        combo(ui, th, "g11.fe.kref", &mut s.f.k_ref, &names);
        if let Ok(cur) = s
            .doc
            .solid_arg(&s.f.k_solid, s.f.k_clause.trim(), s.f.k_arg)
        {
            ui.label(RichText::new(cur).monospace().color(th.c("text.secondary")));
        }
        if btn(ui, th, &cat.t("fe.lock")).clicked() {
            let (so, cl, k, rf) = (
                s.f.k_solid.clone(),
                s.f.k_clause.trim().to_string(),
                s.f.k_arg,
                s.f.k_ref.clone(),
            );
            let r = s.doc.set_solid_arg(&so, &cl, k, &rf);
            s.apply(cat, r);
        }
        if btn(ui, th, &cat.t("fe.unlock")).clicked() {
            let (so, cl, k, rf) = (
                s.f.k_solid.clone(),
                s.f.k_clause.trim().to_string(),
                s.f.k_arg,
                s.f.k_ref.clone(),
            );
            let v = s
                .eval
                .resolved
                .as_ref()
                .and_then(|r| r.num(&rf))
                .unwrap_or(0.0);
            let r = s.doc.set_solid_arg(&so, &cl, k, &mm(v));
            s.apply(cat, r);
        }
    });
    hint(ui, th, cat, "fe.locks.help");
}

fn or0(s: &str) -> &str {
    if s.trim().is_empty() {
        "0"
    } else {
        s.trim()
    }
}

/// Statement heads that define a solid.
const SOLID_HEADS: [&str; 4] = ["extrude", "revolve", "sweep", "blend"];

/// Locale key of a solid definition's kind.
pub fn solid_kind_key(head: &str) -> &'static str {
    match head {
        "extrude" => "fe.solid.extrude",
        "revolve" => "fe.solid.revolve",
        "blend" => "fe.solid.blend",
        _ => "fe.solid.sweep",
    }
}

/// Kinds and label keys of the two bounds of the solid form (revolution: axis position and angle).
pub fn solid_bounds(kind: usize) -> ((Kind, &'static str), (Kind, &'static str)) {
    if kind == 1 {
        ((Kind::Length, "fe.solid.axis"), (Kind::Angle, "fe.solid.angle"))
    } else {
        ((Kind::Length, "fe.solid.z0"), (Kind::Length, "fe.solid.z1"))
    }
}

/// Model value (metres, radians) shown in project units.
pub fn to_display(k: Kind, imperial: bool, v: f64) -> f64 {
    match k {
        Kind::Length if imperial => v / 0.0254,
        Kind::Length => v * 1000.0,
        Kind::Angle => v.to_degrees(),
        _ => v,
    }
}

/// Inverse of `to_display`.
pub fn from_display(k: Kind, imperial: bool, v: f64) -> f64 {
    match k {
        Kind::Length if imperial => v * 0.0254,
        Kind::Length => v / 1000.0,
        Kind::Angle => v.to_radians(),
        _ => v,
    }
}

/// Unit label of a kind in project units.
pub fn unit_of(k: Kind, imperial: bool) -> &'static str {
    match k {
        Kind::Length if imperial => "in",
        Kind::Length => "mm",
        Kind::Angle => "\u{b0}",
        Kind::Area => "m\u{b2}",
        Kind::Volume => "m\u{b3}",
        _ => "",
    }
}

/// Decimals of an input field of a kind in project units.
fn field_decimals(k: Kind, imperial: bool) -> usize {
    match k {
        Kind::Length if imperial => 3,
        Kind::Length | Kind::Angle => 1,
        Kind::Int => 0,
        _ => 4,
    }
}

/// Volume cell of a built part: cubic metres with locale separators.
pub fn volume_cell(v: f64, nf: NumberFormat) -> (String, &'static str) {
    (fmt_num(v, 5, nf), "m\u{b3}")
}

/// Number and unit joined for a sentence.
fn with_unit(text: &str, unit: &str) -> String {
    if unit.is_empty() {
        text.to_string()
    } else {
        format!("{text} {unit}")
    }
}

/// Width of a bound field: two minimum field widths.
const BOUND_W: f32 = ddc_ui::theme::control::FIELD_MIN_W * 2.0;

/// Number field over a model value, shown and edited in project units; `label` is the visible
/// caption widget (its id and text).
#[allow(clippy::too_many_arguments)]
pub(super) fn model_field(
    ui: &mut egui::Ui,
    th: &Theme,
    salt: impl std::hash::Hash,
    value: &mut f64,
    kind: Kind,
    project: &Project,
    nf: NumberFormat,
    label: (egui::Id, &str),
) {
    let mut d = to_display(kind, project.imperial, *value);
    let out = widgets::NumberField::new(salt, &mut d, nf)
        .labelled_by(label.0, label.1)
        .decimals(field_decimals(kind, project.imperial))
        .unit(unit_of(kind, project.imperial))
        .width(BOUND_W)
        .show(ui, th);
    if out.changed || out.committed {
        *value = from_display(kind, project.imperial, d);
    }
}

/// Row to select after a table interaction: a click, or a key move from the current row.
fn picked_row(out: &widgets::TableOutput, cur: Option<usize>, len: usize) -> Option<usize> {
    out.clicked.or_else(|| {
        out.moved.filter(|_| len > 0).map(|d| {
            let at = cur.map_or(-1, |c| c as isize) + d;
            at.clamp(0, len as isize - 1) as usize
        })
    })
}

/// Why a solid cannot be removed (locale key) or None.
pub fn solid_remove_why(selected: bool) -> Option<&'static str> {
    (!selected).then_some("fe.solid.remove_why")
}

/// Solid operator from the form fields; bounds in model units, `kind` indexes the form kinds.
pub fn build_solid_op(kind: usize, a: f64, b: f64, json: &str) -> Result<SolidOp, String> {
    Ok(match kind {
        // profile (r, z) in the vertical plane through the x axis at a; azimuth from +x
        1 => SolidOp {
            form: Form::Revolve {
                profile: BulgeProfile::from_json(json)?.section()?,
                axis_o: v2(0.0, 0.0),
                axis_d: v2(0.0, 1.0),
                start: 0.0,
                end: if b == 0.0 { std::f64::consts::TAU } else { b },
            },
            frame: Frame::vertical(v3(a, 0.0, 0.0), v2(1.0, 0.0)),
        },
        2 => {
            // two profiles: bottom and top, separated by a bar
            let (lo, hi) = json.split_once('|').ok_or("bottom | top")?;
            SolidOp {
                form: Form::Blend {
                    bottom: BulgeProfile::from_json(lo)?.to_ring()?,
                    top: BulgeProfile::from_json(hi)?.to_ring()?,
                    height: b - a,
                },
                frame: Frame::level(a),
            }
        }
        _ => SolidOp {
            form: Form::Extrusion {
                profile: BulgeProfile::from_json(json)?.section()?,
                start: a,
                end: b,
            },
            frame: Frame::default(),
        },
    })
}

fn solids(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
) {
    let nf = NumberFormat::for_lang(cat.lang);
    let all = s.doc.stmts();
    let defs: Vec<_> = f
        .solids
        .iter()
        .filter_map(|x| {
            all.iter()
                .find(|t| t.name == x.name && SOLID_HEADS.contains(&t.head.as_str()))
        })
        .collect();
    let parts = s.eval.built.as_ref().map_or(&[][..], |b| &b.parts[..]);
    if parts.is_empty() && defs.is_empty() {
        crate::ai::ui::kit::empty_state(
            ui,
            th,
            &cat.t("fe.solids.empty"),
            &cat.t("fe.solids.empty_hint"),
            None,
        );
    }
    if !parts.is_empty() {
        let titles = ["fe.col.name", "fe.col.material", "fe.col.volume", "fe.col.state"]
            .map(|h| cat.t(h));
        let cols = [
            text_col(&titles[0]),
            text_col(&titles[1]).fixed(120.0),
            num_col(&titles[2]),
            text_col(&titles[3]).fixed(70.0),
        ];
        let void = cat.t("fe.void");
        let mut sort = None;
        Table::new("g11.fe.solids", &cols, parts.len())
            .max_height(200.0)
            .show(ui, th, &mut sort, |_| RowState::Normal, |r, c| {
                let p = &parts[r];
                match c {
                    0 => Cell::text(p.name.clone()),
                    1 => Cell::text(p.material.clone()),
                    2 => {
                        let (t, u) = volume_cell(p.volume, nf);
                        Cell::num(t, u)
                    }
                    _ if p.void => Cell::muted(void.clone()),
                    _ => Cell::text(String::new()),
                }
            });
    }
    if !defs.is_empty() {
        section_gap(ui);
        let titles = ["fe.col.name", "fe.col.kind", "fe.col.def"].map(|h| cat.t(h));
        let cols = [
            text_col(&titles[0]).fixed(110.0),
            text_col(&titles[1]).fixed(100.0),
            text_col(&titles[2]),
        ];
        let kinds: Vec<String> = defs.iter().map(|d| cat.t(solid_kind_key(&d.head))).collect();
        let mut sort = None;
        let out = Table::new("g11.fe.soliddefs", &cols, defs.len())
            .max_height(200.0)
            .show(
                ui,
                th,
                &mut sort,
                |r| {
                    if s.f.s_sel == defs[r].name {
                        RowState::Selected
                    } else {
                        RowState::Normal
                    }
                },
                |r, c| match c {
                    0 => Cell::text(defs[r].name.clone()),
                    1 => Cell::text(kinds[r].clone()),
                    _ => Cell::text(defs[r].text.clone()).with_tip(defs[r].text.clone()),
                },
            );
        let cur = defs.iter().position(|d| d.name == s.f.s_sel);
        if let Some(d) = picked_row(&out, cur, defs.len()).and_then(|i| defs.get(i)) {
            s.f.s_sel = d.name.clone();
        }
        let sel = defs.iter().find(|d| d.name == s.f.s_sel).copied();
        let why = solid_remove_why(sel.is_some());
        let label = cat.t("fe.remove");
        let r = ui
            .add_enabled_ui(why.is_none(), |ui| Button::danger(&label).show(ui, th))
            .inner;
        let clicked = match why {
            Some(k) => r.on_disabled_hover_text(cat.t(k)).clicked(),
            None => r.clicked(),
        };
        if let (true, Some(st)) = (clicked, sel) {
            let res = s.doc.remove_stmt(st);
            s.apply(cat, res);
        }
    }
    section_gap(ui);
    ui.label(RichText::new(cat.t("fe.solid.form")).strong());
    let kinds = ["fe.solid.extrude", "fe.solid.revolve", "fe.solid.blend"];
    let klabels: Vec<String> = kinds.iter().map(|k| cat.t(k)).collect();
    let ((ka, la), (kb, lb)) = solid_bounds(s.f.s_kind);
    let (la, lb) = (cat.t(la), cat.t(lb));
    let muted = th.c("text.secondary");
    let name_hint = cat.t("fe.col.name");
    ui.horizontal_wrapped(|ui| {
        pick_idx(ui, th, "g11.fe.skind", &mut s.f.s_kind, &klabels);
        widgets::TextField::new(&mut s.f.s_name)
            .id_salt("g11.fe.sname")
            .labelled_by(egui::Id::new("g11.fe.sname.lbl"), &name_hint)
            .hint(&name_hint)
            .width(110.0)
            .show(ui, th);
        let l = (ui.label(RichText::new(la.clone()).color(muted)).id, la.as_str());
        model_field(ui, th, ("g11.fe.sa", s.f.s_kind), &mut s.f.s_a, ka, project, nf, l);
        let l = (ui.label(RichText::new(lb.clone()).color(muted)).id, lb.as_str());
        model_field(ui, th, ("g11.fe.sb", s.f.s_kind), &mut s.f.s_b, kb, project, nf, l);
    });
    egui::CollapsingHeader::new(cat.t("profile_sketch.title"))
        .id_salt("ri7.psk")
        .show(ui, |ui| super::profile_sketch::show(ui, th, cat, s, project));
    hint(ui, th, cat, "fe.solid.profile_help");
    ui.add(
        egui::TextEdit::multiline(&mut s.f.s_json)
            .code_editor()
            .desired_rows(3)
            .desired_width(f32::INFINITY),
    );
    if add_btn(ui, th, &cat.t("fe.add")) {
        match build_solid_op(s.f.s_kind, s.f.s_a, s.f.s_b, &s.f.s_json) {
            Ok(op) => {
                let name = s.f.s_name.trim().to_string();
                let r = s.doc.add_solid(&name, &op, &Own);
                s.apply(cat, r);
            }
            Err(e) => s.msg = Some((true, e)),
        }
    }
}

/// Form state of a saved condition: (always visible, condition text).
pub fn vis_form(saved: &str) -> (bool, String) {
    let c = saved.trim();
    (c.is_empty(), c.to_string())
}

/// Condition written to the model by the visibility form: none means always visible.
pub fn vis_condition(always: bool, cond: &str) -> Option<String> {
    let c = cond.trim();
    (!always && !c.is_empty()).then(|| c.to_string())
}

/// Row of the previously selected target after the rows were rebuilt, and whether its form
/// draft still applies (same target, same saved condition).
pub fn vis_reselect(
    was: Option<&(String, String)>,
    rows: &[(String, String)],
) -> (Option<usize>, bool) {
    let Some((t, saved)) = was else {
        return (None, false);
    };
    match rows.iter().position(|r| &r.0 == t) {
        Some(i) => (Some(i), rows[i].1 == *saved),
        None => (None, false),
    }
}

fn visibility(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session, f: &Family) {
    hint(ui, th, cat, "fe.vis.help");
    if s.f.vis_for != s.doc.text() {
        let was = s.f.vis_sel.and_then(|i| s.f.vis.get(i)).cloned();
        let targets = f
            .solids
            .iter()
            .map(|x| x.name.clone())
            .chain(f.ext.nests.iter().map(|n| n.name.clone()));
        s.f.vis = targets
            .map(|t| {
                let c = s
                    .doc
                    .find("visible", &t)
                    .map(|st| {
                        st.text
                            .splitn(3, char::is_whitespace)
                            .nth(2)
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();
                (t, c)
            })
            .collect();
        s.f.vis_for = s.doc.text().to_string();
        // the selection follows its target, not its row; an unapplied edit stays unless the
        // target's saved condition itself changed
        let (sel, keep) = vis_reselect(was.as_ref(), &s.f.vis);
        s.f.vis_sel = sel;
        if !keep {
            let saved = sel.map(|i| s.f.vis[i].1.clone()).unwrap_or_default();
            (s.f.vis_always, s.f.vis_cond) = vis_form(&saved);
        }
    }
    if s.f.vis.is_empty() {
        crate::ai::ui::kit::empty_state(
            ui,
            th,
            &cat.t("fe.vis.empty"),
            &cat.t("fe.vis.empty_hint"),
            None,
        );
        return;
    }
    let hidden = s
        .eval
        .resolved
        .as_ref()
        .map(|r| r.hidden.clone())
        .unwrap_or_default();
    let titles = ["fe.col.name", "fe.col.state", "fe.col.condition"].map(|h| cat.t(h));
    let cols = [
        text_col(&titles[0]).fixed(140.0),
        text_col(&titles[1]).fixed(90.0),
        text_col(&titles[2]),
    ];
    let (shown_t, hidden_t, always_t) = (
        cat.t("fe.vis.visible"),
        cat.t("fe.vis.hidden"),
        cat.t("fe.vis.always"),
    );
    let mut sort = None;
    let out = Table::new("g11.fe.vis", &cols, s.f.vis.len())
        .max_height(240.0)
        .show(
            ui,
            th,
            &mut sort,
            |r| {
                if s.f.vis_sel == Some(r) {
                    RowState::Selected
                } else {
                    RowState::Normal
                }
            },
            |r, c| {
                let (t, cond) = &s.f.vis[r];
                let off = hidden.contains(t);
                match c {
                    0 if off => Cell::muted(t.clone()),
                    0 => Cell::text(t.clone()),
                    1 if off => Cell::muted(hidden_t.clone()),
                    1 => Cell::text(shown_t.clone()),
                    _ if cond.trim().is_empty() => Cell::muted(always_t.clone()),
                    _ => Cell::text(cond.trim().to_string()).with_tip(cond.trim().to_string()),
                }
            },
        );
    if let Some(i) = picked_row(&out, s.f.vis_sel, s.f.vis.len()) {
        s.f.vis_sel = Some(i);
        (s.f.vis_always, s.f.vis_cond) = vis_form(&s.f.vis[i].1);
    }
    match s.f.vis_sel {
        None => hint(ui, th, cat, "fe.vis.pick"),
        Some(i) => {
            let target = s.f.vis[i].0.clone();
            let wanted = vis_condition(s.f.vis_always, &s.f.vis_cond);
            let saved = vis_condition(false, &s.f.vis[i].1);
            let always_label = cat.t("fe.vis.always_cb");
            ui.label(RichText::new(target.as_str()).strong());
            widgets::checkbox(ui, th, &mut s.f.vis_always, &always_label);
            if !s.f.vis_always {
                let hint_t = cat.t("fe.vis.condition_hint");
                widgets::TextField::new(&mut s.f.vis_cond)
                    .id_salt(("g11.fe.viscond", i))
                    .labelled_by(egui::Id::new("g11.fe.viscond.lbl"), &hint_t)
                    .hint(&hint_t)
                    .show(ui, th);
            }
            if gated_button(ui, th, cat, "fe.apply", text_why(wanted != saved)) {
                let r = s.doc.set_visible(&target, wanted.as_deref());
                s.apply(cat, r);
            }
        }
    }
    let bools: Vec<&str> = f
        .params
        .iter()
        .filter(|p| p.kind == Kind::Bool)
        .map(|p| p.name.as_str())
        .collect();
    if !bools.is_empty() {
        ui.label(
            RichText::new(cat.tf("fe.vis.bools", &[("list", &bools.join(", "))]))
                .color(th.c("text.secondary")),
        );
    }
}

fn nested(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
    acts: &mut Vec<Action>,
) {
    ui.label(RichText::new(cat.t("fe.nest.children")).strong());
    for c in &f.ext.children {
        let types: Vec<&str> = c.types.iter().map(|t| t.name.as_str()).collect();
        // an embedded copy: updates itself when the project family is loaded, here by hand
        let source = project.families.iter().find(|p| p.1 == c.name).map(|p| p.0);
        ui.horizontal(|ui| {
            ui.label(&c.name);
            ui.label(RichText::new(types.join(", ")).color(th.c("text.secondary")));
            if let Some(id) = source {
                if Button::secondary(&cat.t("fe.nest.update"))
                    .small()
                    .show(ui, th)
                    .on_hover_text(cat.t("fe.nest.update_help"))
                    .clicked()
                {
                    acts.push(Action::Embed(id));
                }
            }
            if ddc_ui::widgets::close_button(ui, &cat.t("fe.remove")).clicked() {
                if let Some(st) = s
                    .doc
                    .stmts()
                    .into_iter()
                    .find(|x| x.head == "begin" && x.name == c.name)
                {
                    let r = s.doc.remove_stmt(&st);
                    s.apply(cat, r);
                }
            }
        });
    }
    ui.horizontal(|ui| {
        let others: Vec<&(i64, String)> =
            project.families.iter().filter(|p| p.1 != f.name).collect();
        let names: Vec<String> = others.iter().map(|p| p.1.clone()).collect();
        if let Some(i) = pick_menu(ui, th, &cat.t("fe.nest.embed"), &names) {
            acts.push(Action::Embed(others[i].0));
        }
    });
    hint(ui, th, cat, "fe.nest.help");
    ui.separator();
    ui.label(RichText::new(cat.t("fe.nest.instances")).strong());
    for st in s.doc.stmts().into_iter().filter(|x| x.head == "nest") {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&st.text)
                    .monospace()
                    .size(ddc_ui::theme::type_size::CAPTION),
            );
            if ddc_ui::widgets::close_button(ui, &cat.t("fe.remove")).clicked() {
                let r = s.doc.remove_stmt(&st);
                s.apply(cat, r);
            }
        });
    }
    let children: Vec<String> = f.ext.children.iter().map(|c| c.name.clone()).collect();
    let ftypes: Vec<String> = f
        .params
        .iter()
        .filter(|p| p.kind == Kind::FamilyType)
        .map(|p| p.name.clone())
        .collect();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.f.n_name)
                .hint_text(cat.t("fe.col.name"))
                .desired_width(80.0),
        );
        widgets::checkbox(ui, th, &mut s.f.n_by_param, &cat.t("fe.nest.by_param"));
        if s.f.n_by_param {
            combo(ui, th, "g11.fe.nparam", &mut s.f.n_param, &ftypes);
        } else {
            combo(ui, th, "g11.fe.nchild", &mut s.f.n_child, &children);
            let tys: Vec<String> = f
                .ext
                .children
                .iter()
                .find(|c| c.name == s.f.n_child)
                .map(|c| c.types.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default();
            combo(ui, th, "g11.fe.ntype", &mut s.f.n_type, &tys);
        }
    });
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut s.f.n_at)
                .hint_text(cat.t("fe.nest.at"))
                .desired_width(120.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.f.n_rot)
                .hint_text(cat.t("fe.nest.rot"))
                .desired_width(60.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut s.f.n_set)
                .hint_text(cat.t("fe.nest.set"))
                .desired_width(160.0),
        );
    });
    if add_btn(ui, th, &cat.t("fe.add")) {
        let mut line = format!("nest {} ", s.f.n_name.trim());
        if s.f.n_by_param {
            line += &format!("of({})", s.f.n_param.trim());
        } else {
            line += s.f.n_child.trim();
            if !s.f.n_type.trim().is_empty() {
                line += &format!(" type({})", s.f.n_type.trim());
            }
        }
        line += &format!(" at({})", or0(&s.f.n_at));
        if !s.f.n_rot.trim().is_empty() {
            line += &format!(" rot({})", s.f.n_rot.trim());
        }
        if !s.f.n_set.trim().is_empty() {
            line += &format!(" set({})", s.f.n_set.trim());
        }
        let r = s.doc.add_stmt(&line);
        s.apply(cat, r);
    }
}

/// Trial value typed in project units: none when empty, a text for text kinds, otherwise a plain
/// number in project units or an expression with units. Values are returned in model units.
pub fn trial_value(
    kind: Kind,
    text: &str,
    nf: NumberFormat,
    imperial: bool,
) -> Result<Option<Value>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    if is_text(kind) {
        return Ok(Some(Value::Text(t.trim_matches('"').to_string())));
    }
    if let Some(x) = parse_number(t, nf) {
        return Ok(Some(Value::Num(from_display(kind, imperial, x))));
    }
    parse_num(t).map(|x| Some(Value::Num(x)))
}

/// Why "Try" is unavailable (locale key) or None.
pub fn flex_try_why(invalid: usize) -> Option<&'static str> {
    (invalid > 0).then_some("fe.flex.trial_bad")
}

/// Why "Run" of a sweep is unavailable (locale key) or None.
pub fn sweep_why(param_ok: bool, from: f64, to: f64) -> Option<&'static str> {
    if !param_ok {
        Some("fe.flex.sweep_none")
    } else if !from.is_finite() || !to.is_finite() {
        Some("fe.flex.range_bad")
    } else {
        None
    }
}

fn flex_tab(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    s: &mut Session,
    f: &Family,
    project: &Project,
) {
    let nf = NumberFormat::for_lang(cat.lang);
    let imp = project.imperial;
    hint(ui, th, cat, "fe.flex.help");
    // probe rows are for parameters without formulas
    let free: Vec<&ddc_geom::component::Param> = f
        .params
        .iter()
        .filter(|p| p.formula.is_none() && !f.ext.text_formulas.iter().any(|t| t.0 == p.name))
        .collect();
    if s.flex.trial.len() != free.len()
        || s.flex.trial.iter().zip(&free).any(|(a, p)| a.0 != p.name)
    {
        s.flex.trial = free
            .iter()
            .map(|p| (p.name.clone(), String::new()))
            .collect();
    }
    if free.is_empty() {
        crate::ai::ui::kit::empty_state(
            ui,
            th,
            &cat.t("fe.flex.empty"),
            &cat.t("fe.flex.empty_hint"),
            None,
        );
        return;
    }
    s.flex.sel = s.flex.sel.min(free.len() - 1);
    let parsed: Vec<Result<Option<Value>, String>> = free
        .iter()
        .zip(&s.flex.trial)
        .map(|(p, t)| trial_value(p.kind, &t.1, nf, imp))
        .collect();
    let invalid = parsed.iter().filter(|r| r.is_err()).count();
    let titles = ["fe.col.name", "fe.col.default", "fe.col.trial"].map(|h| cat.t(h));
    let cols = [text_col(&titles[0]), num_col(&titles[1]), num_col(&titles[2])];
    let (keep_t, bad_t) = (cat.t("fe.flex.keep"), cat.t("fe.flex.trial_bad"));
    let mut sort = None;
    let out = Table::new("g11.fe.flex", &cols, free.len())
        .max_height(240.0)
        .show(
            ui,
            th,
            &mut sort,
            |r| {
                if s.flex.sel == r {
                    RowState::Selected
                } else {
                    RowState::Normal
                }
            },
            |r, c| match c {
                0 => Cell::text(free[r].name.clone()),
                1 => {
                    let (t, u) = num_parts(free[r].kind, &free[r].default, imp, nf);
                    if is_text(free[r].kind) {
                        Cell::muted(t)
                    } else {
                        Cell::num(t, u)
                    }
                }
                _ => match &parsed[r] {
                    Err(_) => Cell::text(s.flex.trial[r].1.trim().to_string()).with_tip(bad_t.clone()),
                    Ok(None) => Cell::muted(keep_t.clone()),
                    Ok(Some(v)) => {
                        let (t, u) = num_parts(free[r].kind, v, imp, nf);
                        if is_text(free[r].kind) {
                            Cell::text(t)
                        } else {
                            Cell::num(t, u)
                        }
                    }
                },
            },
        );
    if let Some(i) = picked_row(&out, Some(s.flex.sel), free.len()) {
        s.flex.sel = i;
    }
    {
        let i = s.flex.sel;
        let p = free[i];
        let unit = unit_of(p.kind, imp);
        let label = p.name.clone();
        let hint_t = cat.t("fe.flex.keep");
        let bad = parsed[i].is_err();
        let err_t = cat.t("fe.flex.trial_bad");
        let mut fld = widgets::TextField::new(&mut s.flex.trial[i].1)
            .id_salt(("g11.fe.trial", i))
            .labelled_by(egui::Id::new(("g11.fe.trial.lbl", i)), &label)
            .hint(&hint_t)
            .unit(unit)
            .error(bad)
            .error_text(&err_t)
            .width(160.0);
        if !is_text(p.kind) {
            fld = fld.right_aligned();
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(label.as_str()).color(th.c("text.secondary")));
            fld.show(ui, th);
        });
    }
    let base: Vec<(String, Value)> =
        s.ty.as_deref()
            .and_then(|t| f.type_values(t))
            .map(|v| v.to_vec())
            .or_else(|| f.types.first().map(|t| t.values.clone()))
            .unwrap_or_default();
    ui.horizontal(|ui| {
        if gated_button(ui, th, cat, "fe.flex.try", flex_try_why(invalid)) {
            let trial: Vec<(String, Value)> = parsed
                .iter()
                .zip(&free)
                .filter_map(|(r, p)| match r {
                    Ok(Some(v)) => Some((p.name.clone(), v.clone())),
                    _ => None,
                })
                .collect();
            s.flex.last = Some(flex::try_values(f, &base, &trial));
            s.flex.show = true;
        }
        if btn(ui, th, &cat.t("fe.flex.reset")).clicked() {
            s.flex.last = None;
            s.flex.series.clear();
            s.flex.show = false;
            for t in &mut s.flex.trial {
                t.1.clear();
            }
        }
        widgets::checkbox(ui, th, &mut s.flex.show, &cat.t("fe.flex.show"));
    });
    if let Some(o) = &s.flex.last {
        outcome(ui, th, cat, o, nf);
    }
    section_gap(ui);
    ui.label(RichText::new(cat.t("fe.flex.sweep")).strong());
    let nums: Vec<String> = free
        .iter()
        .filter(|p| !is_text(p.kind))
        .map(|p| p.name.clone())
        .collect();
    if nums.is_empty() {
        hint(ui, th, cat, "fe.flex.sweep_none");
        return;
    }
    if !nums.contains(&s.flex.param) {
        s.flex.param = nums[0].clone();
    }
    let kind = f
        .params
        .iter()
        .find(|p| p.name == s.flex.param)
        .map_or(Kind::Number, |p| p.kind);
    let muted = th.c("text.secondary");
    let (l_from, l_to, l_steps) = (cat.t("fe.flex.from"), cat.t("fe.flex.to"), cat.t("fe.flex.steps"));
    ui.horizontal_wrapped(|ui| {
        combo(ui, th, "g11.fe.sweep", &mut s.flex.param, &nums);
        let l = ui.label(RichText::new(l_from.clone()).color(muted)).id;
        let (fp, fk) = (&s.flex.param, kind);
        let from = (l, l_from.as_str());
        model_field(ui, th, ("g11.fe.from", fp), &mut s.flex.from, fk, project, nf, from);
        let to = (ui.label(RichText::new(l_to.clone()).color(muted)).id, l_to.as_str());
        model_field(ui, th, ("g11.fe.to", fp), &mut s.flex.to, fk, project, nf, to);
        ui.label(RichText::new(l_steps.clone()).color(muted));
        let mut n = s.flex.steps as f64;
        let out = widgets::NumberField::new("g11.fe.steps", &mut n, nf)
            .labelled_by(egui::Id::new("g11.fe.steps.lbl"), &l_steps)
            .range(1.0, 200.0)
            .width(64.0)
            .show(ui, th);
        if out.changed || out.committed {
            s.flex.steps = n.clamp(1.0, 200.0) as usize;
        }
        let why = sweep_why(nums.contains(&s.flex.param), s.flex.from, s.flex.to);
        let label = cat.t("fe.flex.run");
        let r = ui
            .add_enabled_ui(why.is_none(), |ui| Button::primary(&label).show(ui, th))
            .inner;
        let clicked = match why {
            Some(k) => r.on_disabled_hover_text(cat.t(k)).clicked(),
            None => r.clicked(),
        };
        if clicked {
            s.flex.series =
                flex::sweep(f, &base, &s.flex.param, s.flex.from, s.flex.to, s.flex.steps);
        }
    });
    if !s.flex.series.is_empty() {
        match flex::first_failure(&s.flex.series) {
            Some((v, e)) => {
                let (t, u) = num_parts(kind, &Value::Num(v), imp, nf);
                ui.label(
                    RichText::new(cat.tf(
                        "fe.flex.first_failure",
                        &[("value", &with_unit(&t, u)), ("msg", &e.to_string())],
                    ))
                    .color(th.c("error")),
                );
            }
            None => {
                ui.label(RichText::new(cat.t("fe.flex.all_ok")).color(th.c("success")));
            }
        }
        let titles = ["fe.col.value", "fe.col.volume", "fe.col.result"].map(|h| cat.t(h));
        let cols = [num_col(&titles[0]), num_col(&titles[1]), text_col(&titles[2])];
        let builds = cat.t("fe.flex.builds");
        let series = &s.flex.series;
        let mut sort = None;
        Table::new("g11.fe.series", &cols, series.len())
            .max_height(240.0)
            .show(ui, th, &mut sort, |_| RowState::Normal, |r, c| {
                let (v, o) = &series[r];
                match (c, &o.failure, o.volume()) {
                    (0, _, _) => {
                        let (t, u) = num_parts(kind, &Value::Num(*v), imp, nf);
                        Cell::num(t, u)
                    }
                    (1, None, Some(vol)) => {
                        let (t, u) = volume_cell(vol, nf);
                        Cell::num(t, u)
                    }
                    (1, _, _) => Cell::muted("\u{2014}"),
                    (_, Some(e), _) => Cell::text(e.to_string()).with_tip(e.to_string()),
                    _ => Cell::text(builds.clone()),
                }
            });
    }
}

fn outcome(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, o: &flex::Outcome, nf: NumberFormat) {
    match &o.failure {
        Some(e) => {
            ui.label(
                RichText::new(cat.tf("fe.flex.failed", &[("msg", &e.to_string())]))
                    .color(th.c("error")),
            );
        }
        None => {
            let v = o.volume().map(|v| fmt_num(v, 5, nf)).unwrap_or_default();
            ui.label(RichText::new(cat.tf("fe.flex.ok", &[("volume", &v)])).color(th.c("success")));
        }
    }
    if !o.free.is_empty() {
        ui.label(
            RichText::new(cat.tf("fe.verdict.free", &[("list", &o.free.join(", "))]))
                .color(th.c("warning")),
        );
    }
}

fn text(ui: &mut egui::Ui, th: &Theme, cat: &Catalog, s: &mut Session) {
    if s.f.raw_for != s.doc.text() {
        s.f.raw = s.doc.text().to_string();
        s.f.raw_for = s.doc.text().to_string();
    }
    hint(ui, th, cat, "fe.text.help");
    ui.add(
        egui::TextEdit::multiline(&mut s.f.raw)
            .code_editor()
            .desired_rows(24)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        let changed = s.f.raw != s.doc.text();
        if gated_button(ui, th, cat, "fe.apply", text_why(changed)) {
            let r = s.doc.set_text(s.f.raw.clone());
            s.apply(cat, r);
        }
        if gated_button(ui, th, cat, "fe.revert", text_why(changed)) {
            s.f.raw = s.doc.text().to_string();
        }
        if changed {
            if let Some(e) = text_error(&s.f.raw) {
                ui.label(RichText::new(e).color(th.c("error")));
            }
        }
    });
}

/// Error of the edited definition as `line:col: message`, None when it parses.
fn text_error(raw: &str) -> Option<String> {
    ddc_geom::component::ddcf::check(raw).err().map(|e| e.to_string())
}

/// Why "Add" is unavailable (a locale key) or None if it is available.
fn add_why(name: &str, exists: bool) -> Option<&'static str> {
    if name.is_empty() {
        Some("fe.add_why_empty")
    } else if exists {
        Some("fe.add_why_exists")
    } else {
        None
    }
}

/// Why "Apply"/"Delete" parameter are unavailable.
fn param_edit_why(exists: bool) -> Option<&'static str> {
    (!exists).then_some("fe.apply_why_missing")
}

/// Why "Apply"/"Revert" text are unavailable.
fn text_why(changed: bool) -> Option<&'static str> {
    (!changed).then_some("fe.nochange")
}

/// A button disabled when `why = Some(key)`; the reason is shown on hover.
fn gated_button(
    ui: &mut egui::Ui,
    th: &Theme,
    cat: &Catalog,
    label: &str,
    why: Option<&'static str>,
) -> bool {
    let r = ui
        .add_enabled_ui(why.is_none(), |ui| btn(ui, th, &cat.t(label)))
        .inner;
    match why {
        Some(k) => r.on_disabled_hover_text(cat.t(k)).clicked(),
        None => r.clicked(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHY_KEYS: [&str; 4] = [
        "fe.add_why_empty",
        "fe.add_why_exists",
        "fe.apply_why_missing",
        "fe.nochange",
    ];

    #[test]
    fn add_reason_per_state() {
        assert_eq!(add_why("", false), Some("fe.add_why_empty"));
        assert_eq!(add_why("", true), Some("fe.add_why_empty"));
        assert_eq!(add_why("W", true), Some("fe.add_why_exists"));
        assert_eq!(add_why("W", false), None);
    }

    #[test]
    fn edit_reasons_per_state() {
        assert_eq!(param_edit_why(false), Some("fe.apply_why_missing"));
        assert_eq!(param_edit_why(true), None);
        assert_eq!(text_why(false), Some("fe.nochange"));
        assert_eq!(text_why(true), None);
    }

    #[test]
    fn why_keys_in_en_and_ru() {
        let en = include_str!("../../../ui/data/i18n/en/options.json");
        let ru = include_str!("../../../ui/data/i18n/ru/options.json");
        for k in WHY_KEYS {
            let pat = format!("\"{k}\":");
            assert!(en.contains(&pat) && ru.contains(&pat), "{k}");
        }
    }

    fn ui_source() -> &'static str {
        let src = include_str!("tabs.rs");
        src.split("#[cfg(test)]").next().unwrap_or(src)
    }

    #[test]
    fn axis_labels_in_en_and_ru_for_every_variant() {
        use ddc_ui::{Catalog, Lang};
        let cat = Catalog::new(Lang::En);
        for a in AXES {
            let key = axis_key(a);
            for lang in [Lang::En, Lang::Ru] {
                let t = cat.table(lang).get(key).and_then(|v| v.as_str());
                assert!(t.is_some_and(|t| !t.is_empty()), "{key} {lang:?}");
            }
            assert_ne!(cat.t_in(Lang::Ru, key), cat.t_in(Lang::En, key), "{key}");
        }
        assert_eq!(axis_key(Axis::X), "fe.axis.x");
        assert_eq!(axis_key(Axis::Y), "fe.axis.y");
        assert_eq!(axis_key(Axis::Z), "fe.axis.z");
    }

    #[test]
    fn new_keys_in_en_and_ru() {
        let en = include_str!("../../../ui/data/i18n/en/family_editor_axes.json");
        let ru = include_str!("../../../ui/data/i18n/ru/family_editor_axes.json");
        for k in [
            "fe.col.axis",
            "fe.col.at",
            "fe.col.mode",
            "fe.mode.fixed",
            "fe.mode.free",
            "fe.types.inherited",
            "fe.csv.more",
            "fe.ref.selected",
        ] {
            let pat = format!("\"{k}\":");
            assert!(en.contains(&pat) && ru.contains(&pat), "{k}");
        }
    }

    #[test]
    fn number_format_trims_and_localizes() {
        let (en, ru) = (NumberFormat::EN, NumberFormat::RU);
        assert_eq!(fmt_num(600.0, 3, en), "600");
        assert_eq!(fmt_num(1234.5, 3, en), "1,234.5");
        assert_eq!(fmt_num(1234.5, 3, ru), "1\u{a0}234,5");
        assert_eq!(fmt_num(0.25, 3, ru), "0,25");
        assert_eq!(fmt_num(-0.0001, 3, en), "0");
        assert_eq!(fmt_num(7.0, 0, en), "7");
    }

    #[test]
    fn numeric_columns_are_right_aligned() {
        assert_eq!(num_col("W").align, egui::Align::Max);
        assert_eq!(text_col("Name").align, egui::Align::Min);
    }

    #[test]
    fn unit_suffix_follows_project_setting() {
        let en = NumberFormat::EN;
        let len = Value::Num(0.0254);
        assert_eq!(num_parts(Kind::Length, &len, false, en), ("25.4".into(), "mm"));
        assert_eq!(num_parts(Kind::Length, &len, true, en), ("1".into(), "in"));
        assert_eq!(num_parts(Kind::Area, &Value::Num(2.5), false, en), ("2.5".into(), "m\u{b2}"));
        assert_eq!(num_parts(Kind::Volume, &Value::Num(0.5), true, en), ("0.5".into(), "m\u{b3}"));
        let a = num_parts(Kind::Angle, &Value::Num(std::f64::consts::FRAC_PI_2), false, en);
        assert_eq!(a, ("90".into(), "\u{b0}"));
        assert_eq!(num_parts(Kind::Int, &Value::Num(3.0), false, en), ("3".into(), ""));
        assert_eq!(num_parts(Kind::Text, &Value::Text("Oak".into()), true, en), ("Oak".into(), ""));
    }

    #[test]
    fn cell_kind_typed_or_inherited() {
        let v = Value::Num(1.0);
        assert_eq!(cell_kind(Some(&v)), CellKind::Typed);
        assert_eq!(cell_kind(None), CellKind::Inherited);
    }

    #[test]
    fn tabs_use_kit_widgets_only() {
        let src = ui_source();
        for bad in [
            "egui::ComboBox",
            "ui.checkbox(",
            "ui.button(",
            "small_button(",
            "egui::Button::new",
            "selectable_label(",
            "selectable_value(",
            "{:?}",
        ] {
            assert!(!src.contains(bad), "{bad}");
        }
    }

    #[test]
    fn solids_visibility_flex_have_no_grid_or_raw_inputs() {
        let src = ui_source();
        assert!(!src.contains("egui::Grid"));
        let a = src.find("fn solids(").unwrap();
        let b = src.find("fn nested(").unwrap();
        let c = src.find("fn flex_tab(").unwrap();
        let d = src.find("fn text(").unwrap();
        for part in [&src[a..b], &src[c..d]] {
            for bad in ["TextEdit::singleline", "DragValue", "{:.5}", "{:?}"] {
                assert!(!part.contains(bad), "{bad}");
            }
        }
    }

    #[test]
    fn display_units_follow_project_and_roundtrip() {
        assert_eq!(to_display(Kind::Length, false, 0.5), 500.0);
        assert!((to_display(Kind::Length, true, 0.0254) - 1.0).abs() < 1e-12);
        assert!((to_display(Kind::Angle, false, std::f64::consts::PI) - 180.0).abs() < 1e-9);
        assert_eq!(to_display(Kind::Number, true, 2.5), 2.5);
        for (k, imp) in [(Kind::Length, false), (Kind::Length, true), (Kind::Angle, false)] {
            let back = from_display(k, imp, to_display(k, imp, 0.37));
            assert!((back - 0.37).abs() < 1e-12, "{k:?} {imp}");
        }
        assert_eq!(unit_of(Kind::Length, false), "mm");
        assert_eq!(unit_of(Kind::Length, true), "in");
        assert_eq!(unit_of(Kind::Angle, true), "\u{b0}");
        assert_eq!(unit_of(Kind::Text, false), "");
        assert_eq!(field_decimals(Kind::Length, true), 3);
        assert_eq!(field_decimals(Kind::Length, false), 1);
    }

    #[test]
    fn volume_cell_localizes_and_trims() {
        assert_eq!(volume_cell(0.125, NumberFormat::EN), ("0.125".into(), "m\u{b3}"));
        assert_eq!(volume_cell(0.125, NumberFormat::RU), ("0,125".into(), "m\u{b3}"));
        assert_eq!(volume_cell(1234.5, NumberFormat::RU).0, "1\u{a0}234,5");
        assert_eq!(volume_cell(0.0, NumberFormat::EN).0, "0");
        assert_eq!(num_parts(Kind::Length, &Value::Num(0.6), false, NumberFormat::RU).0, "600");
        assert_eq!(num_parts(Kind::Length, &Value::Num(0.0125), false, NumberFormat::RU).0, "12,5");
        assert_eq!(with_unit("12", "mm"), "12 mm");
        assert_eq!(with_unit("3", ""), "3");
    }

    fn num_of(r: Result<Option<Value>, String>) -> f64 {
        match r {
            Ok(Some(Value::Num(x))) => x,
            _ => f64::NAN,
        }
    }

    #[test]
    fn trial_values_parse_in_project_units() {
        let (en, ru) = (NumberFormat::EN, NumberFormat::RU);
        assert!(matches!(trial_value(Kind::Length, "  ", en, false), Ok(None)));
        assert!((num_of(trial_value(Kind::Length, "600", en, false)) - 0.6).abs() < 1e-12);
        assert!((num_of(trial_value(Kind::Length, "2", en, true)) - 0.0508).abs() < 1e-12);
        assert!((num_of(trial_value(Kind::Length, "12,5", ru, false)) - 0.0125).abs() < 1e-12);
        assert!((num_of(trial_value(Kind::Angle, "90", en, false)) - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
        assert!((num_of(trial_value(Kind::Number, "2.5", en, false)) - 2.5).abs() < 1e-12);
        assert!((num_of(trial_value(Kind::Length, "600mm", en, false)) - 0.6).abs() < 1e-12);
        assert!(trial_value(Kind::Length, "abc", en, false).is_err());
        assert!(matches!(
            trial_value(Kind::Material, "\"Oak\"", en, false),
            Ok(Some(Value::Text(t))) if t == "Oak"
        ));
    }

    #[test]
    fn try_and_sweep_are_gated_with_reasons() {
        assert_eq!(flex_try_why(0), None);
        assert_eq!(flex_try_why(2), Some("fe.flex.trial_bad"));
        assert_eq!(sweep_why(false, 0.0, 1.0), Some("fe.flex.sweep_none"));
        assert_eq!(sweep_why(true, f64::NAN, 1.0), Some("fe.flex.range_bad"));
        assert_eq!(sweep_why(true, 0.0, f64::INFINITY), Some("fe.flex.range_bad"));
        assert_eq!(sweep_why(true, 0.1, 0.9), None);
    }

    #[test]
    fn visibility_toggle_maps_to_model_condition() {
        assert_eq!(vis_condition(true, "Open"), None);
        assert_eq!(vis_condition(false, "  "), None);
        assert_eq!(vis_condition(false, " Open "), Some("Open".into()));
        assert_eq!(vis_form(""), (true, String::new()));
        assert_eq!(vis_form(" W > 1m "), (false, "W > 1m".into()));
        // load then write back is the identity for both states
        for saved in ["", "Open", "W > 1m"] {
            let (always, cond) = vis_form(saved);
            assert_eq!(vis_condition(always, &cond), vis_condition(false, saved));
        }
    }

    #[test]
    fn visibility_selection_follows_its_target_after_a_rebuild() {
        let row = |t: &str, c: &str| (t.to_string(), c.to_string());
        let was = row("Door", "Open");
        // a solid added in front moves the row: the selection moves with it, the draft stays
        let rows = [row("Box", ""), row("Door", "Open")];
        assert_eq!(vis_reselect(Some(&was), &rows), (Some(1), true));
        // its saved condition changed: the form reloads
        let rows = [row("Door", "W > 1m")];
        assert_eq!(vis_reselect(Some(&was), &rows), (Some(0), false));
        assert_eq!(vis_reselect(Some(&was), &[row("Box", "")]), (None, false));
        assert_eq!(vis_reselect(None, &rows), (None, false));
    }

    #[test]
    fn solid_form_kinds_bounds_and_errors() {
        let ((ka, la), (kb, lb)) = solid_bounds(0);
        assert_eq!((ka, la, kb, lb), (Kind::Length, "fe.solid.z0", Kind::Length, "fe.solid.z1"));
        let ((ka, la), (kb, lb)) = solid_bounds(1);
        assert_eq!((ka, la, kb, lb), (Kind::Length, "fe.solid.axis", Kind::Angle, "fe.solid.angle"));
        assert_eq!(solid_bounds(2).1 .1, "fe.solid.z1");
        assert_eq!(solid_kind_key("extrude"), "fe.solid.extrude");
        assert_eq!(solid_kind_key("sweep"), "fe.solid.sweep");
        assert_eq!(solid_remove_why(false), Some("fe.solid.remove_why"));
        assert_eq!(solid_remove_why(true), None);
        assert_eq!(build_solid_op(2, 0.0, 1.0, "{}").err().as_deref(), Some("bottom | top"));
        assert!(build_solid_op(0, 0.0, 1.0, "not json").is_err());
        assert!(build_solid_op(1, 0.0, 0.0, "").is_err());
    }

    #[test]
    fn row_pick_clicks_and_keys_clamp() {
        let none = widgets::TableOutput::default();
        assert_eq!(picked_row(&none, Some(1), 3), None);
        let click = widgets::TableOutput { clicked: Some(2), ..Default::default() };
        assert_eq!(picked_row(&click, None, 3), Some(2));
        let down = widgets::TableOutput { moved: Some(1), ..Default::default() };
        assert_eq!(picked_row(&down, Some(2), 3), Some(2));
        assert_eq!(picked_row(&down, None, 3), Some(0));
        let up = widgets::TableOutput { moved: Some(-1), ..Default::default() };
        assert_eq!(picked_row(&up, Some(0), 3), Some(0));
        assert_eq!(picked_row(&down, Some(0), 0), None);
    }

    #[test]
    fn ux49g_keys_in_en_and_ru() {
        use ddc_ui::{Catalog, Lang};
        let en = include_str!("../../../ui/data/i18n/en/family_editor_flex.json");
        let ru = include_str!("../../../ui/data/i18n/ru/family_editor_flex.json");
        let cat = Catalog::new(Lang::En);
        let keys = [
            "fe.col.material", "fe.col.volume", "fe.col.state", "fe.col.def", "fe.col.condition",
            "fe.col.default", "fe.col.trial", "fe.col.result", "fe.solid.sweep",
            "fe.solid.remove_why", "fe.solids.empty", "fe.solids.empty_hint", "fe.vis.visible",
            "fe.vis.hidden", "fe.vis.always_cb", "fe.vis.condition_hint", "fe.vis.empty",
            "fe.vis.empty_hint", "fe.vis.pick", "fe.flex.empty", "fe.flex.empty_hint",
            "fe.flex.keep", "fe.flex.trial_bad", "fe.flex.sweep_none", "fe.flex.range_bad",
            "fe.flex.steps", "fe.flex.builds",
        ];
        for k in keys {
            let pat = format!("\"{k}\":");
            assert!(en.contains(&pat) && ru.contains(&pat), "{k}");
            assert_ne!(cat.t_in(Lang::Ru, k), cat.t_in(Lang::En, k), "{k}");
        }
        for head in SOLID_HEADS {
            let k = solid_kind_key(head);
            assert!(!cat.t_in(Lang::En, k).is_empty() && !cat.t_in(Lang::Ru, k).is_empty(), "{k}");
        }
    }

    #[test]
    fn no_cyrillic_in_ui_code() {
        for (n, line) in ui_source().lines().enumerate() {
            let code = line.split("//").next().unwrap_or(line);
            assert!(
                !code.chars().any(|c| ('\u{400}'..='\u{4ff}').contains(&c)),
                "line {}",
                n + 1
            );
        }
    }

    #[test]
    fn text_error_has_line_and_column() {
        assert_eq!(text_error("family A\nparam W length type 1m\n"), None);
        let e = text_error("family A\n  param W lenth type 1m\n").unwrap();
        assert!(e.starts_with("2:11: ") && e.contains("lenth"), "{e}");
        let e = text_error("ddcf 2\nfamily A\nversion one\n").unwrap();
        assert!(e.starts_with("3:9: "), "{e}");
        assert_eq!(text_error("ddcf 2\nfamily A\nversion 1.0.0\nuid 00c0ffee\n"), None);
    }

    #[test]
    fn param_tails_quote_texts_only() {
        assert_eq!(param_tail(Kind::Length, "600mm"), "600mm");
        assert_eq!(param_tail(Kind::Material, "Oak"), "\"Oak\"");
        assert_eq!(
            param_tail(Kind::Material, "= if(W > 1m, \"A\", \"B\")"),
            "= if(W > 1m, \"A\", \"B\")"
        );
        assert_eq!(value_text(Kind::Length, &Value::Num(0.6)), "600mm");
        assert_eq!(value_text(Kind::Text, &Value::Text("x".into())), "\"x\"");
    }

    /// b16 shot: the insertion point inputs sat on a third line and were cut off at the
    /// default window height. Title, axes and Apply share one row; the help is on hover.
    #[test]
    fn insertion_point_title_and_inputs_share_one_row() {
        let src = ui_source();
        let at = src.find("cat.t(\"fe.origin\")").expect("origin title");
        let row = src[..at].rfind("ui.horizontal_wrapped(").expect("one row");
        let block = &src[row..];
        let block = &block[..block.find("\n    });").expect("row end")];
        assert!(block.contains("AXES.iter()") && block.contains("fe.apply"), "{block}");
        assert!(!block.contains("hint(ui"), "the help is a hover, not a line");
        assert!(src.contains("on_hover_text(&help)"));
    }
}
