// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Drawing set: what to put on the sheets, the sheet list it will make, and Apply as one undo
//! step that tags rooms, doors and windows, lays the sheets out and fills the title blocks.
//! A second run updates the same sheets.

use super::props::pick_row;
use super::*;
use ddc_io::sheets::DsetOptions;
use ddc_ui::widgets;

const SCALES: [&str; 6] = ["50", "100", "200", "250", "400", "500"];
const PAPERS: [&str; 4] = ["A3", "A2", "A1", "A0"];
const STANDARDS: [&str; 2] = ["gost", "iso"];

/// The dialog state; `scale` and `paper` 0 is automatic.
pub struct DsetDialog {
    plans: bool,
    sections: bool,
    elevations: bool,
    schedules: bool,
    scale: usize,
    paper: usize,
    standard: usize,
    /// Sheet number and name of the preview, with the summary line; refreshed on change.
    preview: Option<Result<(String, Vec<(String, String)>), String>>,
    err: String,
}

impl Default for DsetDialog {
    fn default() -> Self {
        Self {
            plans: true,
            sections: true,
            elevations: true,
            schedules: true,
            scale: 0,
            paper: 0,
            standard: 0,
            preview: None,
            err: String::new(),
        }
    }
}

impl DsetDialog {
    /// The title block follows the interface language: GOST in Russian, ISO in English.
    pub fn for_lang(ru: bool) -> Self {
        Self {
            standard: usize::from(!ru),
            ..Self::default()
        }
    }

    pub fn options(&self) -> DsetOptions {
        DsetOptions {
            plans: self.plans,
            sections: self.sections,
            elevations: self.elevations,
            schedules: self.schedules,
            scale: (self.scale > 0).then(|| SCALES[self.scale - 1].parse().unwrap_or(100.0)),
            paper: (self.paper > 0).then(|| PAPERS[self.paper - 1].to_string()),
            standard: STANDARDS[self.standard].to_string(),
        }
    }
}

fn sheet_error(e: io::SheetError) -> ddc_model::Error {
    ddc_model::Error::Invalid(e.0)
}

/// Model schedules a set carries, as the schedule window shows them.
fn set_tables(ed: &Editor) -> Vec<(String, io::Table)> {
    io::SCHEDULES
        .iter()
        .filter_map(|k| schedule_table(ed, k).map(|t| (k.to_string(), t)))
        .collect()
}

/// The plan for the model as it stands, words in the UI language of `ru`.
pub fn plan_drawing_set(ed: &Editor, o: &DsetOptions, ru: bool) -> Result<io::Planned, String> {
    plan_drawing_set_in(ed, o, if ru { Lang::Ru } else { Lang::En })
}

/// Same, words in any UI language.
pub fn plan_drawing_set_in(ed: &Editor, o: &DsetOptions, lang: Lang) -> Result<io::Planned, String> {
    with_ctx_in(ed, lang, None, None, None, |ctx| io::plan_drawing_set(&ed.model, o, ctx)).map_err(|e| e.0)
}

/// Doors and windows the set will mark by type: those without a mark on the levels it tags.
pub fn marks_to_assign(ed: &Editor) -> Result<usize, String> {
    let lv = io::tag_levels(&ed.model).map_err(|e| e.0)?;
    ed.model.unmarked_openings(&lv).map_err(|e| e.to_string())
}

/// Door and window mark prefixes in a UI language.
pub fn mark_prefix(lang: Lang) -> ddc_model::MarkPrefix {
    io::mark_prefix(&|k: &str| msg_in(lang, k, &[]))
}

/// Tags, plans and writes the set in one undo step; the sheet ids in order. Sheet words and
/// tags in the project's document language; a refusal in the UI language `ru`.
pub fn build_drawing_set(ed: &mut Editor, o: DsetOptions, ru: bool) -> Result<Vec<i64>, String> {
    let tables = set_tables(ed);
    let units = lu(ed.tool.units);
    let lang = crate::doc_lang::doc_lang(&ed.model);
    op_in(ed, ru, move |m| {
        m.atomic("drawing_set", |m| {
            let lv = io::tag_levels(m).map_err(sheet_error)?;
            let text = |k: &str| msg_in(lang, k, &[]);
            m.tag_for_drawings(&lv, &io::mark_prefix(&text))?;
            let sched = |k: &str| tables.iter().find(|t| t.0 == k).map(|t| t.1.clone());
            let ctx = Ctx {
                units,
                schedules: &sched,
                index: None,
                paper: None,
                text: &text,
                refs: None,
            };
            let p = io::plan_drawing_set(m, &o, &ctx).map_err(sheet_error)?;
            m.apply_drawing_set(&p.plan)?;
            Ok(())
        })
    })?;
    let mut ids = ed.model.drawing_set_sheets().map_err(|e| e.to_string())?;
    let order = ed.model.sheets().map_err(|e| e.to_string())?;
    ids.sort_by_key(|(id, _)| order.iter().position(|s| s.0 == *id));
    Ok(ids.into_iter().map(|(id, _)| id).collect())
}

fn preview(
    ed: &Editor,
    d: &DsetDialog,
    ru: bool,
) -> Result<(String, Vec<(String, String)>), String> {
    let p = plan_drawing_set_in(ed, &d.options(), crate::doc_lang::doc_lang(&ed.model))?;
    let rows: Vec<(String, String)> = p
        .plan
        .sheets
        .iter()
        .map(|s| (s.def.number.clone(), s.def.name.clone()))
        .collect();
    let n = rows.len().to_string();
    let (plan, views) = (
        format!("{:.0}", p.plan_scale),
        format!("{:.0}", p.view_scale),
    );
    let args = [
        ("n", n.as_str()),
        ("plan", plan.as_str()),
        ("views", views.as_str()),
        ("paper", p.paper.as_str()),
    ];
    let mut line = msg(ru, "dset.dlg.summary", &args);
    let marks = marks_to_assign(ed)?;
    if marks > 0 {
        let n = marks.to_string();
        line = format!("{line}
{}", msg(ru, "dset.marks_assigned", &[("n", &n)]));
    }
    for w in &p.warnings {
        line = format!("{line}\n{w}");
    }
    Ok((line, rows))
}

pub(super) fn cmd_drawing_set(app: &mut EditorApp) {
    let ru = app.sh.cat.lang == Lang::Ru;
    with(|s| s.dset = Some(DsetDialog::for_lang(ru)));
}

impl DsetDialog {
    /// Builds the preview when an option changed since the last one.
    pub fn refresh(&mut self, ed: &Editor, ru: bool) {
        if self.preview.is_none() {
            self.preview = Some(preview(ed, self, ru));
        }
    }

    /// Sheets the preview lists; `None` while it is missing or failed.
    pub fn sheet_count(&self) -> Option<usize> {
        match &self.preview {
            Some(Ok((_, rows))) => Some(rows.len()),
            _ => None,
        }
    }

    /// The preview's line: sheets, scales and paper, then any warnings.
    pub fn summary(&self) -> Option<&str> {
        match &self.preview {
            Some(Ok((line, _))) => Some(line),
            _ => None,
        }
    }

    /// Why no set can be made, from the preview.
    pub fn preview_error(&self) -> Option<&str> {
        match &self.preview {
            Some(Err(e)) => Some(e),
            _ => None,
        }
    }

    /// Content, scale, paper and title block, then the preview; the preview resets on a change.
    pub fn controls(&mut self, ui: &mut egui::Ui, ru: bool) {
        options_ui(ui, ru, self);
        preview_ui(ui, ru, self);
    }
}

fn options_ui(ui: &mut egui::Ui, ru: bool, d: &mut DsetDialog) {
    let auto = msg(ru, "dset.dlg.auto", &[]);
    let scales: Vec<String> = std::iter::once(auto.clone())
        .chain(SCALES.iter().map(|s| format!("1:{s}")))
        .collect();
    let papers: Vec<String> = std::iter::once(auto)
        .chain(PAPERS.iter().map(|s| s.to_string()))
        .collect();
    let stds = [
        msg(ru, "dset.dlg.std_gost", &[]),
        msg(ru, "dset.dlg.std_iso", &[]),
    ];
    let th = ddc_ui::theme::of(ui.ctx());
    let mut changed = false;
    ui.label(RichText::new(msg(ru, "dset.dlg.content", &[])).strong());
    for (on, key) in [
        (&mut d.plans, "dset.dlg.plans"),
        (&mut d.sections, "dset.dlg.sections"),
        (&mut d.elevations, "dset.dlg.elevations"),
        (&mut d.schedules, "dset.dlg.schedules"),
    ] {
        changed |= widgets::checkbox(ui, &th, on, &msg(ru, key, &[])).changed();
    }
    ui.add_space(space::SM);
    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }
    let rows = [
        ("dset.dlg.scale", "dset.scale", &mut d.scale, refs(&scales)),
        ("dset.dlg.paper", "dset.paper", &mut d.paper, refs(&papers)),
        ("dset.dlg.standard", "dset.standard", &mut d.standard, refs(&stds)),
    ];
    for (label, salt, at, items) in rows {
        changed |= pick_row(ui, &th, &msg(ru, label, &[]), salt, at, &items, "");
    }
    if changed {
        d.preview = None;
    }
}

fn preview_ui(ui: &mut egui::Ui, ru: bool, d: &DsetDialog) {
    let th = ddc_ui::theme::of(ui.ctx());
    ui.add_space(space::SM);
    ui.label(RichText::new(msg(ru, "dset.dlg.preview", &[])).strong());
    match &d.preview {
        Some(Ok((line, rows))) => {
            ui.label(RichText::new(line).color(th.sem().text_secondary));
            egui::ScrollArea::vertical()
                .id_salt("dset.preview")
                .max_height(space::XXXL * 5.0)
                .show(ui, |ui| {
                    for (n, name) in rows {
                        ui.label(format!("{n}  {name}"));
                    }
                });
        }
        Some(Err(e)) => {
            ui.label(RichText::new(e).color(th.sem().error));
        }
        None => {}
    }
}

/// The dialog; false when it closes.
pub(super) fn dset_window(
    ctx: &egui::Context,
    app: &mut EditorApp,
    ru: bool,
    d: &mut DsetDialog,
) -> bool {
    let (mut keep, mut open, mut apply) = (true, true, false);
    d.refresh(&app.ed, ru);
    kit::Dialog::new(
        "drawing_set",
        msg(ru, "dset.dlg.title", &[]),
        kit::DialogSize::Form,
    )
    .open(&mut open)
    .show(ctx, |ui| {
        let th = ddc_ui::theme::of(ui.ctx());
        d.controls(ui, ru);
        if !d.err.is_empty() {
            ui.label(RichText::new(&d.err).color(th.sem().error));
        }
        kit::dialog_footer(ui, |ui| {
            let ready = matches!(d.preview, Some(Ok(_)));
            let tip = (!ready).then_some("");
            if ready && kit::primary_action(ui, msg(ru, "dset.dlg.apply", &[]), tip) {
                apply = true;
            }
            if kit::cancel_button(ui, msg(ru, "sheets.cancel", &[])).clicked() {
                keep = false;
            }
        });
    });
    if apply {
        let marks = marks_to_assign(&app.ed);
        let built = marks.and_then(|k| {
            build_drawing_set(&mut app.ed, d.options(), ru).map(|ids| (k, ids))
        });
        match built {
            Ok((marks, ids)) => {
                let n = ids.len().to_string();
                let mut m = msg(ru, "dset.dlg.done", &[("n", &n)]);
                if marks > 0 {
                    let k = marks.to_string();
                    m = format!("{m}; {}", msg(ru, "dset.marks_assigned", &[("n", &k)]));
                }
                app.sh.toasts.push(Level::Info, m, None);
                if let Some(&first) = ids.first() {
                    with(|s| open_sheet(s, first));
                }
                keep = false;
            }
            Err(e) => d.err = e,
        }
    }
    keep && open
}
