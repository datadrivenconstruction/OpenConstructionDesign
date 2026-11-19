// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! XLSX round trips over every showcase project (ddc-model tests/showcase): an untouched book,
//! metric and imperial, imports with no change at all; one number edited per sheet kind is
//! exactly one change, applies, survives a re-export and undoes to the state before it.

#[path = "../../model/tests/showcase/mod.rs"]
mod showcase;

use std::path::Path;

use ddc_io::xlsx::{self, ImportOptions, Report, Units};
use ddc_model::Model;
use showcase::{count, dump, each, first_id, remove_db, same, temp_file, valid, Keep};

const FT: f64 = 0.3048;

fn opts(apply: bool) -> ImportOptions {
    ImportOptions {
        apply,
        delete_missing: false,
        new_params: false,
    }
}

fn export(m: &Model, p: &Path, u: Units) -> Result<(), String> {
    xlsx::export_xlsx_units(m, p, u).map_err(|e| format!("export {u:?}: {e}"))
}

/// Everything a preview reports besides warnings.
fn noise(r: &Report) -> String {
    let mut s = String::new();
    if !r.changes.is_empty() {
        let c: Vec<String> = r
            .changes
            .iter()
            .take(5)
            .map(|c| {
                format!("{}!{}:{} {:?} '{}'->'{}'", c.sheet, c.row, c.column, c.id, c.old, c.new)
            })
            .collect();
        s += &format!(" {} changes {c:?}", r.changes.len());
    }
    if !r.rejected.is_empty() {
        s += &format!(" rejected {:?}", r.rejected.iter().take(5).collect::<Vec<_>>());
    }
    if !r.edits.is_empty() {
        s += &format!(" {} edits", r.edits.len());
    }
    if !r.props.is_empty() {
        s += &format!(" {} props", r.props.len());
    }
    if !r.conflicts.is_empty() {
        s += &format!(" {} conflicts", r.conflicts.len());
    }
    s
}

fn untouched(m: &mut Model, name: &str, u: Units) -> Result<(), String> {
    let p = temp_file(&format!("{name}_{u:?}.xlsx"));
    export(m, &p, u)?;
    let before = dump(m, Keep::All);
    let r = (|| {
        let pre = xlsx::import_xlsx_with(m, &p, opts(false))?;
        let n = noise(&pre);
        if !n.is_empty() {
            return Err(format!("{u:?} preview of an untouched book:{n}"));
        }
        let ap = xlsx::import_xlsx_with(m, &p, opts(true))?;
        let n = noise(&ap);
        if !n.is_empty() {
            return Err(format!("{u:?} apply of an untouched book:{n}"));
        }
        same(&dump(m, Keep::All), &before, &format!("{u:?} untouched book applied"))
    })();
    remove_db(&p);
    r
}

#[test]
fn an_untouched_book_changes_nothing() {
    each(|p, mut m| {
        untouched(&mut m, p.name, Units::Metric)?;
        untouched(&mut m, p.name, Units::Imperial)
    });
}

/// One editable number of one sheet: the row's id, the model value and the edit.
struct Case {
    sheet: &'static str,
    header: &'static str,
    /// The row to edit; projects without one skip the case.
    id: &'static str,
    /// The value in the model, `?1` = the row id.
    value: &'static str,
    delta: f64,
    /// A length: shown in feet in an imperial book.
    length: bool,
}

const fn case(
    sheet: &'static str,
    header: &'static str,
    id: &'static str,
    value: &'static str,
    delta: f64,
    length: bool,
) -> Case {
    Case { sheet, header, id, value, delta, length }
}

const CASES: &[Case] = &[
    case(
        "Levels",
        "elevation [m]",
        "SELECT id FROM level ORDER BY elevation DESC, id LIMIT 1",
        "SELECT elevation FROM level WHERE id=?1",
        0.25,
        true,
    ),
    case(
        "Walls",
        "height [m]",
        "SELECT id FROM wall ORDER BY id LIMIT 1",
        "SELECT height FROM wall WHERE id=?1",
        0.2,
        true,
    ),
    case(
        "Openings",
        "height [m]",
        "SELECT id FROM opening WHERE kind='window' ORDER BY id LIMIT 1",
        "SELECT height FROM opening WHERE id=?1",
        -0.1,
        true,
    ),
    case(
        "Rooms",
        "height [m]",
        "SELECT id FROM room ORDER BY id LIMIT 1",
        "SELECT height FROM room WHERE id=?1",
        -0.1,
        true,
    ),
    case(
        "Slabs",
        "offset [m]",
        "SELECT id FROM slab ORDER BY id LIMIT 1",
        "SELECT offset FROM slab WHERE id=?1",
        -0.05,
        true,
    ),
    case(
        "Curtain walls",
        "height [m]",
        "SELECT id FROM curtain_wall ORDER BY id LIMIT 1",
        "SELECT height FROM curtain_wall WHERE id=?1",
        -0.2,
        true,
    ),
    // a layer row is keyed by its type: one-layer types only
    case(
        "Layers",
        "thickness [m]",
        "SELECT wtype FROM wall_type_layer GROUP BY wtype HAVING count(*)=1 ORDER BY wtype LIMIT 1",
        "SELECT thickness FROM wall_type_layer WHERE wtype=?1",
        0.01,
        true,
    ),
    case(
        "SlabLayers",
        "thickness [m]",
        "SELECT stype FROM slab_type_layer GROUP BY stype HAVING count(*)=1 ORDER BY stype LIMIT 1",
        "SELECT thickness FROM slab_type_layer WHERE stype=?1",
        0.01,
        true,
    ),
    case(
        "Materials",
        "density [kg/m3]",
        "SELECT id FROM material WHERE density IS NOT NULL ORDER BY id LIMIT 1",
        "SELECT density FROM material WHERE id=?1",
        10.0,
        false,
    ),
];

fn value(m: &Model, c: &Case, id: i64) -> f64 {
    m.sql().query_row(c.value, [id], |r| r.get(0)).unwrap()
}

/// Book number of a model value.
fn shown(c: &Case, v: f64, u: Units) -> f64 {
    if c.length && u == Units::Imperial {
        v / FT
    } else {
        v
    }
}

fn one_edit(m: &mut Model, name: &str, c: &Case, u: Units) -> Result<(), String> {
    let Some(id) = first_id(m, c.id) else {
        return Ok(());
    };
    let ctx = format!("{} {} {u:?}", c.sheet, c.header);
    let src = temp_file(&format!("{name}_{}_{u:?}.xlsx", c.sheet.replace(' ', "_")));
    let dst = temp_file(&format!("{name}_{}_{u:?}_edit.xlsx", c.sheet.replace(' ', "_")));
    let r = (|| {
        let v0 = value(m, c, id);
        export(m, &src, u)?;
        // control: the same number written back is no change
        xlsx::patch_cell(&src, &dst, c.sheet, id, c.header, shown(c, v0, u))?;
        let pre = xlsx::import_xlsx_with(m, &dst, opts(false))?;
        let n = noise(&pre);
        if !n.is_empty() {
            return Err(format!("{ctx}: same value rewritten:{n}"));
        }
        let before = dump(m, Keep::User);
        let ops = count(m, "op");
        let v1 = v0 + c.delta;
        xlsx::patch_cell(&src, &dst, c.sheet, id, c.header, shown(c, v1, u))?;
        let pre = xlsx::import_xlsx_with(m, &dst, opts(false))?;
        let hit = pre.changes.iter().all(|x| x.sheet == c.sheet && x.id == Some(id));
        if pre.changes.len() != 1 || !hit || !pre.rejected.is_empty() {
            return Err(format!("{ctx}: preview of one edit:{}", noise(&pre)));
        }
        let ap = xlsx::import_xlsx_with(m, &dst, opts(true))?;
        if !ap.rejected.is_empty() {
            return Err(format!("{ctx}: apply rejected {:?}", ap.rejected));
        }
        let got = value(m, c, id);
        if (got - v1).abs() > 1e-6 {
            return Err(format!("{ctx}: applied {got}, wanted {v1}"));
        }
        valid(m, &format!("{ctx} applied"))?;
        export(m, &src, u)?;
        let again = xlsx::import_xlsx_with(m, &src, opts(false))?;
        let n = noise(&again);
        if !n.is_empty() {
            return Err(format!("{ctx}: re-export after the edit:{n}"));
        }
        while count(m, "op") > ops {
            m.undo().map_err(|e| format!("{ctx}: undo: {e}"))?;
        }
        if (value(m, c, id) - v0).abs() > 1e-9 {
            return Err(format!("{ctx}: undo left {}, wanted {v0}", value(m, c, id)));
        }
        same(&dump(m, Keep::User), &before, &format!("{ctx} undone"))
    })();
    remove_db(&src);
    remove_db(&dst);
    r
}

#[test]
fn one_edit_per_sheet_kind_round_trips() {
    each(|p, mut m| {
        let mut bad = vec![];
        for u in [Units::Metric, Units::Imperial] {
            for c in CASES {
                if let Err(e) = one_edit(&mut m, p.name, c, u) {
                    bad.push(e);
                }
            }
        }
        if bad.is_empty() {
            Ok(())
        } else {
            Err(bad.join("\n  "))
        }
    });
}

#[test]
fn every_sheet_kind_is_edited_somewhere() {
    let projects: Vec<Model> = showcase::PROJECTS.iter().map(|p| showcase::build(p.name)).collect();
    for c in CASES {
        assert!(
            projects.iter().any(|m| first_id(m, c.id).is_some()),
            "no project has a row for {}",
            c.sheet
        );
    }
}
