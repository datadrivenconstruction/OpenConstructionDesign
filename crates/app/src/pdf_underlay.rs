// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! `mgr.import_pdf` ("PDF underlay"): picks `.pdf` files and opens the "Import drawing" form
//! with them, each on the level its file name names (else the active one) and the PDF scale
//! field live. Placing goes through the drawing import every underlay uses.

use crate::shell::EditorApp;
use crate::status::Severity;
use std::path::PathBuf;

fn is_pdf(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
}

/// Opens the import form with `files`. A file that is not a `.pdf` refuses all of them.
pub fn open_files(a: &mut EditorApp, files: Vec<PathBuf>) -> bool {
    if files.is_empty() {
        return false;
    }
    if let Some(bad) = files.iter().find(|p| !is_pdf(p)) {
        let name = crate::ui_kit::file_name_of(&bad.to_string_lossy());
        a.ed.say(Severity::Refusal, a.sh.cat.tf("pdfu.not_pdf", &[("name", &name)]));
        return false;
    }
    a.open_drawing_files(files);
    true
}

/// The native picker, `.pdf` only; None when cancelled or blocked (screenshots, tests).
fn pick(d: crate::file_pick::Dlg, cat: &ddc_ui::Catalog) -> Option<Vec<PathBuf>> {
    d.set_title(cat.t("pdfu.pick_title")).add_filter(cat.t("pdfu.filter"), &["pdf"]).pick_files()
}

fn run(a: &mut EditorApp) {
    if a.io_busy() {
        a.ed.say(Severity::Refusal, a.sh.cat.t("job.busy"));
        return;
    }
    if let Some(files) = pick(crate::file_pick::Dlg::new(), &a.sh.cat) {
        open_files(a, files);
    }
}

pub fn register(a: &mut EditorApp) {
    a.register_command("mgr.import_pdf", run);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use ddc_io::pdf::{export_pdf, Item, Sheet, SheetLayer};

    fn app() -> EditorApp {
        EditorApp::new(Editor::new_in_memory().unwrap(), None, "test".into())
    }

    /// An A3 sheet with one 100 mm line: 10 m at 1:100.
    fn plan_pdf(dir: &std::path::Path) -> PathBuf {
        let sheet = Sheet {
            width_mm: 420.0,
            height_mm: 297.0,
            layers: vec![SheetLayer { name: "A-WALL".into(), visible: true }],
            items: vec![Item::Polyline {
                pts: vec![[50.0, 50.0], [150.0, 50.0]],
                closed: false,
                weight_mm: 0.35,
                rgb: [0, 0, 0],
                layer: 0,
            }],
        };
        let p = dir.join("Level 1 plan.pdf");
        std::fs::write(&p, export_pdf(&[sheet]).unwrap()).unwrap();
        p
    }

    #[test]
    fn the_command_is_wired_and_a_blocked_picker_opens_nothing() {
        let a = app();
        assert!(a.handlers.contains_key("mgr.import_pdf"));
        let cat = ddc_ui::Catalog::new(ddc_ui::Lang::En);
        assert!(pick(crate::file_pick::Dlg::with_blocked(true), &cat).is_none());
        for lang in [ddc_ui::Lang::En, ddc_ui::Lang::Ru] {
            let cat = ddc_ui::Catalog::new(lang);
            for k in ["pdfu.pick_title", "pdfu.filter", "pdfu.not_pdf"] {
                assert_ne!(cat.t(k), k, "{lang:?} {k}");
            }
        }
    }

    #[test]
    fn a_pdf_is_placed_on_the_level_at_the_sheet_scale() {
        let dir = std::env::temp_dir().join(format!("ddc_pdfu_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = plan_pdf(&dir);
        let mut a = app();
        let level = a.ed.level;
        let ops = a.ed.model.op_top().unwrap();
        assert!(open_files(&mut a, vec![p.clone()]));
        assert!(a.plan.drawings.import_open);
        let (file, pick) = a.drawing_form.files[0].clone();
        assert_eq!(file, p);
        assert_eq!(pick, crate::shell::drawing_win::LevelPick::Existing(level));
        let ratio = a.drawing_form.pdf_ratio;
        assert_eq!(ratio, 100.0);
        let read = crate::drawing::read_underlay_with(&file, None, ratio, 0, &|| false);
        assert!(read.is_ok(), "{:?}", read.as_ref().err());
        let files = std::mem::take(&mut a.drawing_form.files);
        a.place_read(files.into_iter().map(|(f, l)| (f, l, read.clone())).collect());
        assert_eq!(a.plan.drawings.list.len(), 1);
        let d = &a.plan.drawings.list[0];
        assert_eq!(d.level, level);
        let (lo, hi) = d.bbox().expect("lines placed");
        let w = hi.x - lo.x;
        assert!((w - 10.0).abs() < 0.01, "100 mm at 1:100 is 10 m, got {w}");
        // an underlay is view data, not a model edit: the model journal stays untouched
        assert_eq!(a.ed.model.op_top().unwrap(), ops);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_a_pdf_is_refused_with_its_name() {
        let mut a = app();
        assert!(!open_files(&mut a, vec![PathBuf::from("plan.dxf")]));
        assert!(!a.plan.drawings.import_open);
        let said = a.ed.status.clone().unwrap_or_default();
        assert!(said.contains("plan.dxf"), "{said}");
    }
}
