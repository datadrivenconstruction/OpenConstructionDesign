//! A workbook names the project it was exported from (Info "project"). Element ids are per
//! project file, so a book of another project is refused before it is read: no preview is
//! computed against this model and Apply stays off. A book that names no project (older or
//! hand-made) is read as before, with a muted note.
use super::xlsx_win::XlsxPreview;
use crate::ui_kit as kit;
use ddc_io::xlsx::{self, Report};
use ddc_model::Model;
use std::path::Path;

/// English report text of the refusal; the preview's error line holds it while refused.
pub const FOREIGN: &str = "workbook exported from another project";

/// Where a book comes from, compared with the open project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Same,
    Other,
    /// The book or the model names no project, or the book cannot be opened (the read
    /// reports that itself).
    Unknown,
}

pub fn model_uid(model: &Model) -> Option<i64> {
    model.sql().query_row("SELECT value FROM meta WHERE key='project_uid'", [], |r| r.get(0)).ok()
}

pub fn origin(model: &Model, path: &Path) -> Origin {
    origin_of(model, xlsx::book_project(path).ok().flatten())
}

/// [`origin`] for a project uid already read from a book (schedule books keep it elsewhere).
pub fn origin_of(model: &Model, book: Option<i64>) -> Origin {
    match (model_uid(model), book) {
        (Some(m), Some(b)) if m == b => Origin::Same,
        (Some(_), Some(_)) => Origin::Other,
        _ => Origin::Unknown,
    }
}

/// Gate of every preview read: `Err(FOREIGN)` for a book of another project.
pub fn check(model: &Model, path: &Path) -> Result<(), String> {
    match origin(model, path) {
        Origin::Other => Err(FOREIGN.into()),
        _ => Ok(()),
    }
}

pub fn is_foreign(pv: &XlsxPreview) -> bool {
    pv.error.as_deref() == Some(FOREIGN)
}

/// The preview of a refused book: no report, nothing to apply, never synced.
pub fn refused(path: &Path, delete_missing: bool) -> XlsxPreview {
    let mut pv = XlsxPreview::linked(path, delete_missing);
    pv.synced = false;
    pv.synced_at = None;
    pv.error = Some(FOREIGN.into());
    pv
}

/// A re-read hit the gate (the linked file was replaced by another project's book): the old
/// report goes too, so nothing of it can be applied.
pub(super) fn drop_report_if_foreign(pv: &mut XlsxPreview, e: &str) {
    if e != FOREIGN {
        return;
    }
    pv.report = Report::default();
    pv.report_gen += 1;
    pv.rev = None;
    pv.synced = false;
    pv.confirm_mass = false;
}

/// Muted line of a book that names no project; nothing for other books.
pub(super) fn note(ui: &mut egui::Ui, r: &Report, ru: bool) {
    if r.no_project {
        kit::elided_weak(ui, crate::tr_str(ru, "xlsx_project.no_project"));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    ExportNew,
    Close,
}

pub fn body(path: &Path, ru: bool) -> String {
    let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    crate::tr_lang(ru, "xlsx_project.body", &[("file", &file)])
}

/// Blocking card of a refused book: export this project anew, or close. No "use anyway".
pub(super) fn card(ctx: &egui::Context, path: &Path, ru: bool) -> Option<Click> {
    let mut out = None;
    kit::Dialog::new("xlsx_foreign_book", crate::tr_str(ru, "xlsx_project.title"), kit::DialogSize::Form)
        .id(egui::Id::new("xlsx_foreign_book"))
        .show(ctx, |ui| {
            ui.add(egui::Label::new(body(path, ru)).wrap());
            ui.add_space(ddc_ui::theme::space::MD);
            ui.horizontal(|ui| {
                if kit::primary_button(ui, crate::tr_str(ru, "xlsx_project.export_new"), true).clicked() {
                    out = Some(Click::ExportNew);
                }
                if kit::cancel_button(ui, crate::tr_str(ru, "xlsx_project.close")).clicked() {
                    out = Some(Click::Close);
                }
            });
        });
    if out.is_none() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        out = Some(Click::Close);
    }
    out
}

#[cfg(test)]
#[path = "xlsx_project_tests.rs"]
mod tests;
