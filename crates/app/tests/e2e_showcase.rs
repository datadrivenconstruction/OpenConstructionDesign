// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Every showcase project (ddc-model tests/showcase) loaded into the editor: it shows all of
//! its elements, the editor's book (with the family sheets) comes back untouched with no
//! change, metric and imperial, and one Walls edit applied as the XLSX window does undoes in
//! one step back to the state before it.

#[path = "../../model/tests/showcase/mod.rs"]
mod showcase;

use ddc_app::editor::Editor;
use ddc_app::elem::sheets::families;
use ddc_io::xlsx::{self, ImportOptions, Units};
use ddc_model::Model;
use showcase::{count, dump, each, first_id, remove_db, same, temp_file, valid, Keep};

const PREVIEW: ImportOptions = ImportOptions {
    apply: false,
    delete_missing: false,
    new_params: false,
};

const CORE: &[&str] = &["level", "wall", "opening", "room", "slab", "comp", "curtain_wall"];

fn counts(m: &Model) -> Vec<(&'static str, i64)> {
    CORE.iter()
        .map(|&t| (t, if showcase::has_table(m, t) { count(m, t) } else { 0 }))
        .collect()
}

fn load(name: &str, m: Model) -> Result<Editor, String> {
    let want = counts(&m);
    let mut ed = Editor::new_in_memory().map_err(|e| e.to_string())?;
    ed.replace_model(m).map_err(|e| format!("load: {e}"))?;
    let got = counts(&ed.model);
    if got != want {
        return Err(format!("{name} in the editor: {got:?}, built {want:?}"));
    }
    valid(&ed.model, "in the editor")?;
    Ok(ed)
}

fn untouched(ed: &mut Editor, name: &str, u: Units) -> Result<(), String> {
    let fams = families();
    let p = temp_file(&format!("{name}_fam_{u:?}.xlsx"));
    let r = (|| {
        xlsx::export_xlsx_families(&ed.model, &p, u, &fams)?;
        let rep = xlsx::import_xlsx_families(&mut ed.model, &p, PREVIEW, &fams)?;
        if !rep.changes.is_empty() || !rep.props.is_empty() || !rep.rejected.is_empty() {
            let c: Vec<String> = rep
                .changes
                .iter()
                .take(5)
                .map(|c| format!("{}:{} '{}'->'{}'", c.sheet, c.column, c.old, c.new))
                .collect();
            return Err(format!(
                "{u:?} untouched family book: {} changes {c:?}, {} props, {} rejected",
                rep.changes.len(),
                rep.props.len(),
                rep.rejected.len()
            ));
        }
        Ok(())
    })();
    remove_db(&p);
    r
}

fn wall_edit(ed: &mut Editor, name: &str) -> Result<(), String> {
    let Some(id) = first_id(&ed.model, "SELECT id FROM wall ORDER BY id LIMIT 1") else {
        return Ok(());
    };
    let height = |m: &Model| -> f64 {
        m.sql().query_row("SELECT height FROM wall WHERE id=?1", [id], |r| r.get(0)).unwrap()
    };
    let fams = families();
    let src = temp_file(&format!("{name}_walls.xlsx"));
    let dst = temp_file(&format!("{name}_walls_edit.xlsx"));
    let r = (|| {
        let h0 = height(&ed.model);
        xlsx::export_xlsx_families(&ed.model, &src, Units::Metric, &fams)?;
        xlsx::patch_cell(&src, &dst, "Walls", id, "height [m]", h0 + 0.3)?;
        let rep = xlsx::import_xlsx_families(&mut ed.model, &dst, PREVIEW, &fams)?;
        if rep.changes.len() != 1 || !rep.rejected.is_empty() {
            return Err(format!("preview: {} changes, {:?}", rep.changes.len(), rep.rejected));
        }
        let before = dump(&ed.model, Keep::User);
        ed.apply_xlsx_props(rep.batch(), rep.props.clone(), "showcase")?;
        if (height(&ed.model) - h0 - 0.3).abs() > 1e-6 {
            return Err(format!("applied height {}", height(&ed.model)));
        }
        valid(&ed.model, "wall edit applied")?;
        if !ed.can_undo() {
            return Err("the Apply is no undo step".into());
        }
        ed.undo();
        same(&dump(&ed.model, Keep::User), &before, "wall edit undone")
    })();
    remove_db(&src);
    remove_db(&dst);
    r
}

#[test]
fn every_project_loads_into_the_editor() {
    each(|p, m| load(p.name, m).map(|_| ()));
}

#[test]
fn an_untouched_family_book_changes_nothing() {
    each(|p, m| {
        let mut ed = load(p.name, m)?;
        untouched(&mut ed, p.name, Units::Metric)?;
        untouched(&mut ed, p.name, Units::Imperial)
    });
}

#[test]
fn a_wall_edit_from_xlsx_undoes_in_one_step() {
    each(|p, m| {
        let mut ed = load(p.name, m)?;
        wall_edit(&mut ed, p.name)
    });
}
