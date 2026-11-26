// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sheet properties window: number, format, stamp, title-block fields, revisions.

use super::*;
use ddc_ui::widgets::{self, Cell, Column, RowState, Table, TextField};

/// Sheet properties window (a new sheet has `id` None).
pub struct Props {
    pub id: Option<i64>,
    pub def: SheetDef,
    pub revs: Vec<Revision>,
    revs0: Vec<Revision>,
    err: String,
    /// Revision row open in the fields under the table.
    pub(super) sel: Option<usize>,
}

impl Props {
    pub(super) fn load(ed: &Editor, def: SheetDef, id: Option<i64>) -> Self {
        let revs = id
            .and_then(|i| ed.model.revisions(i).ok())
            .unwrap_or_default();
        Props {
            id,
            def,
            revs0: revs.clone(),
            revs,
            err: String::new(),
            sel: None,
        }
    }
}

/// Fields that sheet assembly fills in itself when the user leaves them empty.
const AUTO: [&str; 8] = [
    "title",
    "scale",
    "sheet",
    "sheets",
    "identification",
    "issue_date",
    "revision",
    "date",
];

fn stamp_fields(standard: &str, ru: bool) -> Vec<(&'static str, &'static str)> {
    let f = |k, key| (k, crate::tr_str(ru, key));
    if standard == "gost" {
        vec![
            f("designation", "shv.fld.iso.designation"),
            f("title", "shv.fld.iso.title"),
            f("organization", "shv.fld.iso.organization"),
            f("stage", "shv.fld.iso.stage"),
            f("developer", "shv.fld.iso.developer"),
            f("checker", "shv.fld.iso.checker"),
            f("norm_control", "shv.fld.iso.norm_control"),
            f("tech_control", "shv.fld.iso.tech_control"),
            f("approver", "shv.fld.iso.approver"),
            f("date", "shv.fld.iso.date"),
            f("sheet", "shv.fld.iso.sheet"),
            f("sheets", "shv.fld.iso.sheets"),
            f("scale", "shv.fld.iso.scale"),
            f("mass", "shv.fld.iso.mass"),
            f("material", "shv.fld.iso.material"),
        ]
    } else {
        vec![
            f("title", "shv.fld.iso.title"),
            f("identification", "shv.fld.iso.identification"),
            f("legal_owner", "shv.fld.iso.legal_owner"),
            f("department", "shv.fld.iso.department"),
            f("tech_ref", "shv.fld.iso.tech_ref"),
            f("creator", "shv.fld.iso.creator"),
            f("approver", "shv.fld.iso.approver"),
            f("document_type", "shv.fld.iso.document_type"),
            f("status", "shv.fld.iso.status"),
            f("revision", "shv.fld.iso.revision"),
            f("issue_date", "shv.fld.iso.issue_date"),
            f("sheet", "shv.fld.iso.sheet"),
            f("scale", "shv.fld.iso.scale"),
        ]
    }
}

pub(super) fn props_window(
    ctx: &egui::Context,
    ed: &mut Editor,
    ru: bool,
    p: &mut Props,
) -> Option<Option<i64>> {
    let title = if p.id.is_some() {
        tr(ru, "shv.sheet_properties")
    } else {
        tr(ru, "shv.new_sheet_2")
    };
    let mut out = None;
    let mut open = true;
    kit::Dialog::new("sheet_properties", title, kit::DialogSize::Tool)
        .stable()
        .open(&mut open)
        .show(ctx, |ui| {
            kit::pinned_footer(ui, |ui, footer| {
                let blocked = number_block(ed, ru, p.id, &p.def.number);
                if footer {
                    if kit::primary_action_table(ui, tr(ru, "shv.ok"), blocked.as_deref()) {
                        match save_props(ed, ru, p) {
                            Ok(id) => out = Some(Some(id)),
                            Err(e) => p.err = e,
                        }
                    }
                    if kit::cancel_button(ui, tr(ru, "shv.cancel")).clicked() {
                        out = Some(None);
                    }
                    let th = ddc_ui::theme::of(ui.ctx());
                    if widgets::Button::quiet(&msg(ru, "sheets.project.open", &[]))
                        .show(ui, &th)
                        .clicked()
                    {
                        with(|s| s.open_project = true);
                    }
                    return;
                }
                let th = ddc_ui::theme::of(ui.ctx());
                let th = &*th;
                let bad_number = number_block(ed, ru, p.id, &p.def.number).is_some();
                text_row(ui, th, tr(ru, "shv.number"), "sheet.props.number", &mut p.def.number, "", bad_number);
                text_row(ui, th, tr(ru, "shv.name_2"), "sheet.props.name", &mut p.def.name, "", false);
                let mut at = PAPERS.iter().position(|s| *s == p.def.paper).unwrap_or(0);
                let size = paper_note(&p.def.paper, p.def.landscape, ru);
                if pick_row(ui, th, tr(ru, "shv.paper"), "sheet.paper", &mut at, &PAPERS, &size) {
                    p.def.paper = PAPERS[at].to_string();
                }
                let orient = [tr(ru, "shv.landscape_2"), tr(ru, "shv.portrait_2")];
                let mut at = usize::from(!p.def.landscape);
                if pick_row(ui, th, tr(ru, "sheet_props.orientation"), "sheet.orient", &mut at, &orient, "") {
                    p.def.landscape = at == 0;
                }
                let stds = ["ISO 7200", tr(ru, "shv.gost_r_21_101")];
                let mut at = usize::from(p.def.standard == "gost");
                if pick_row(ui, th, tr(ru, "shv.title_block"), "sheet.std", &mut at, &stds, "") {
                    p.def.standard = if at == 1 { "gost" } else { "iso" }.to_string();
                }
                widgets::section(ui, th, tr(ru, "shv.title_block_fields"), |ui| {
                    for (k, label) in stamp_fields(&p.def.standard, ru) {
                        let v = p.def.fields.entry(k.to_string()).or_default();
                        let hint = if AUTO.contains(&k) { tr(ru, "shv.auto") } else { "" };
                        text_row(ui, th, label, &format!("sheet.props.f.{k}"), v, hint, false);
                    }
                });
                widgets::section(ui, th, tr(ru, "shv.revisions"), |ui| {
                    revisions_block(ui, th, ru, p);
                });
                if !p.err.is_empty() {
                    kit::error_note(ui, &p.err);
                } else if let Some(why) = &blocked {
                    kit::error_note(ui, why);
                }
            });
        });
    if !open {
        out = Some(None);
    }
    out
}

/// Labelled text field on the shared label column; the field takes the value column.
pub(super) fn text_row(
    ui: &mut egui::Ui,
    th: &Theme,
    key: &str,
    salt: &str,
    buf: &mut String,
    hint: &str,
    bad: bool,
) -> widgets::field::FieldOutput {
    // a dialog form row: the field takes the width the label column leaves (names read whole)
    widgets::form::field_row(ui, th, key, kit::ROW_LABEL_W, |ui, lid| {
        let w = ui.available_width();
        TextField::new(buf)
            .id_salt(salt)
            .hint(hint)
            .labelled_by(lid, key)
            .width(w)
            .error(bad)
            .show(ui, th)
    })
}

/// Labelled dropdown; `note` is a muted caption on the right of the control. True when the pick moved.
pub(super) fn pick_row(
    ui: &mut egui::Ui,
    th: &Theme,
    key: &str,
    salt: &str,
    at: &mut usize,
    items: &[&str],
    note: &str,
) -> bool {
    widgets::form::field_row(ui, th, key, kit::ROW_LABEL_W, |ui, _| {
        let room = if note.is_empty() {
            0.0
        } else {
            widgets::form::text_w(ui, th, note) + ui.spacing().item_spacing.x
        };
        let w = (ui.available_width() - room).max(ddc_ui::theme::control::DROPDOWN_W);
        let changed = widgets::dropdown(ui, th, salt, at, items, Some(w)).changed();
        if !note.is_empty() {
            widgets::form::unit(ui, th, note);
        }
        changed
    })
}

/// Paper size in mm for the dropdown note, e.g. "420 × 297 mm".
fn paper_note(paper: &str, landscape: bool, ru: bool) -> String {
    let mm = crate::units::unit_label_in(crate::units::Units::Metric, crate::units::lang_of(ru));
    io::paper_size(paper, landscape)
        .map(|(w, h)| format!("{w:.0} \u{d7} {h:.0} {mm}"))
        .unwrap_or_default()
}

/// Revision table columns; weights, not widths, so the table follows the dialog.
pub(super) fn rev_cols(ru: bool) -> [Column<'static>; 4] {
    [
        Column::text(tr(ru, "shv.rev")).fill(0.9).sortable(false),
        Column::text(tr(ru, "shv.date")).fill(1.3).sortable(false),
        Column::text(tr(ru, "shv.description")).fill(2.8).sortable(false),
        Column::text(tr(ru, "shv.by")).fill(1.4).sortable(false),
    ]
}

/// Read-only revision table, the selected row's fields below it, quiet Add.
fn revisions_block(ui: &mut egui::Ui, th: &Theme, ru: bool, p: &mut Props) {
    if p.revs.is_empty() {
        ui.label(RichText::new(tr(ru, "sheet_props.rev.none")).color(th.sem().text_secondary));
    } else {
        let cols = rev_cols(ru);
        let sel = p.sel;
        let revs = &p.revs;
        let mut sort = None;
        let out = Table::new("sheet.props.r", &cols, revs.len())
            .max_height(th.d().row * 6.0)
            .show(
                ui,
                th,
                &mut sort,
                |i| {
                    if Some(i) == sel {
                        RowState::Selected
                    } else {
                        RowState::Normal
                    }
                },
                |i, c| {
                    let r = &revs[i];
                    match c {
                        0 => Cell::text(r.mark.clone()),
                        1 => Cell::muted(r.date.clone()),
                        2 => Cell::text(r.description.clone()),
                        _ => Cell::muted(r.author.clone()),
                    }
                },
            );
        if let Some(i) = out.clicked {
            p.sel = Some(i);
        }
    }
    let mut drop = None;
    if let Some(i) = p.sel.filter(|i| *i < p.revs.len()) {
        ui.add_space(space::XS);
        let r = &mut p.revs[i];
        text_row(ui, th, tr(ru, "shv.rev"), "sheet.props.r.mark", &mut r.mark, "", false);
        text_row(ui, th, tr(ru, "shv.date"), "sheet.props.r.date", &mut r.date, "", false);
        text_row(ui, th, tr(ru, "shv.description"), "sheet.props.r.desc", &mut r.description, "", false);
        text_row(ui, th, tr(ru, "shv.by"), "sheet.props.r.by", &mut r.author, "", false);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ctx = ui.ctx().clone();
            let tip = tr(ru, "sheet_props.rev.delete");
            let hit = widgets::with_icons(&ctx, |icons| {
                widgets::icon_button_danger(ui, icons, th, "delete", tip)
            });
            if hit.clicked() {
                drop = Some(i);
            }
        });
    }
    if let Some(i) = drop {
        p.revs.remove(i);
        p.sel = None;
    }
    if widgets::Button::quiet(tr(ru, "shv.revision")).show(ui, th).clicked() {
        p.revs.push(Revision {
            mark: (p.revs.len() + 1).to_string(),
            ..Default::default()
        });
        p.sel = Some(p.revs.len() - 1);
    }
}

/// Why OK is off for this number (empty, or used by another sheet), in the UI language.
pub(super) fn number_block(
    ed: &Editor,
    ru: bool,
    id: Option<i64>,
    number: &str,
) -> Option<String> {
    let number = number.trim();
    if number.is_empty() {
        return Some(msg(ru, "sheets.err.no_number", &[]));
    }
    let other = ed
        .model
        .sheets()
        .ok()?
        .into_iter()
        .find(|(i, s)| Some(*i) != id && s.number == number)?;
    Some(msg(
        ru,
        "sheets.err.number_taken",
        &[("number", number), ("name", other.1.name.as_str())],
    ))
}

/// Sheet properties and revisions are one operation (a single undo step, no partial result).
pub(super) fn save_props(ed: &mut Editor, ru: bool, p: &Props) -> Result<i64, String> {
    let mut def = p.def.clone();
    def.number = def.number.trim().to_string();
    def.fields.retain(|_, v| !v.trim().is_empty());
    if def.number.is_empty() {
        return Err(msg(ru, "sheets.err.no_number", &[]));
    }
    let revs: Vec<Revision> = p
        .revs
        .iter()
        .filter(|r| !(r.mark.trim().is_empty() && r.description.trim().is_empty()))
        .cloned()
        .collect();
    let def_changed =
        p.id.is_none_or(|id| ed.model.sheet(id).ok().as_ref() != Some(&def));
    if def_changed || revs != p.revs0 {
        let (id, d) = (p.id, def.clone());
        op_in(ed, ru, move |m| {
            m.save_sheet_with_revisions(id, &d, &revs).map(|_| ())
        })?;
    }
    match p.id {
        Some(id) => Ok(id),
        // the sheet number is unique: the new sheet is found by it
        None => ed
            .model
            .sheets()
            .map_err(|e| err_text(ru, &e))?
            .into_iter()
            .find(|s| s.1.number == def.number)
            .map(|s| s.0)
            .ok_or_else(|| msg(ru, "sheets.err.not_created", &[])),
    }
}

impl Props {
    /// New sheet modelled on the open one (format, stamp, constant fields), with the next number.
    pub(super) fn new_with(ed: &Editor, ru: bool, active: Option<i64>) -> Self {
        let tpl = active
            .and_then(|id| ed.model.sheet(id).ok())
            .map(|mut s| {
                s.fields.retain(|k, _| !AUTO.contains(&k.as_str()));
                s
            })
            .unwrap_or_else(|| SheetDef::new("", ""));
        Props {
            id: None,
            def: SheetDef {
                number: ed
                    .model
                    .next_sheet_number()
                    .unwrap_or_else(|_| "A-101".into()),
                name: msg(ru, "sheets.default_name", &[]),
                ..tpl
            },
            revs: vec![],
            revs0: vec![],
            err: String::new(),
            sel: None,
        }
    }
}
